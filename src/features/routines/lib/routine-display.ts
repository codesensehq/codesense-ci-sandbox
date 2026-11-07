// SPDX-License-Identifier: MIT
import { eventTriggerWords } from "./routine-sentence";
import type {
  RoutineAgentDisplay,
  RoutineDef,
  RoutineLastRun,
  RoutineRow,
  RoutineRun,
  RoutineRunPhase,
  RoutineRunsOn,
} from "@/tauri/commands";

/** The list's groups, in the order they render: Unassigned (its agent is nowhere on this
 *  Mac, or another routine of its space has its id, so it never fires until fixed) first,
 *  then Mine and Team. */
export interface RoutineGroups {
  unassigned: RoutineRow[];
  mine: RoutineRow[];
  team: RoutineRow[];
}

/** Rows ordered by name (then key, so equal names never swap): the order depends
 *  only on the routines themselves, never on selection or on what ran last. */
function byName(a: RoutineRow, b: RoutineRow): number {
  return (
    a.def.name.localeCompare(b.def.name, undefined, { sensitivity: "base" }) ||
    a.key.localeCompare(b.key)
  );
}

/** Split rows into the Unassigned ones, then the routines the caller created and their
 *  teammates'. Every routine is editable by anyone; the split only says who made it. */
export function groupRoutines(rows: readonly RoutineRow[]): RoutineGroups {
  const sorted = [...rows].sort(byName);
  const held = (r: RoutineRow) => r.unassigned || r.duplicate;
  const assigned = sorted.filter((r) => !held(r));
  return {
    unassigned: sorted.filter(held),
    mine: assigned.filter((r) => r.mine),
    team: assigned.filter((r) => !r.mine),
  };
}

/** A routine's agent as a person reads it: its name, "(archived)" once archived. One
 *  that does not resolve reads as the reference as written; the caller marks it. */
export function agentName(agent: RoutineAgentDisplay): string {
  return agent.archived ? `${agent.name} (archived)` : agent.name;
}

/** A routine's schedule as it opens a line: sentence case, so "weekdays at
 *  9:00" and "every 2 hours" read the same in the list and the detail. */
export function scheduleLabel(row: Pick<RoutineRow, "schedule_in_words">): string {
  const words = row.schedule_in_words;
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** A list row keeps its first reason and the count; detail headers keep every word. */
export function routineListLabel(row: Pick<RoutineRow, "def" | "schedule_in_words">): string {
  const triggers = row.def.triggers ?? [];
  if (row.def.after) return scheduleLabel(row);
  if (!triggers.length) return scheduleLabel(row);
  if (triggers.length === 1) {
    const only = triggers[0];
    if (!("event" in only)) return scheduleLabel(row);
    const words = eventTriggerWords(only.event);
    return words.charAt(0).toUpperCase() + words.slice(1);
  }
  const first = triggers[0];
  const words =
    "event" in first ? eventTriggerWords(first.event) : row.schedule_in_words.split("; ")[0];
  return `${words.charAt(0).toUpperCase() + words.slice(1)} + ${triggers.length - 1} more`;
}

/** Where a routine runs, in words. */
export function whereLabel(kind: string): string {
  if (kind === "team_workspace") return "Team workspace";
  if (kind.startsWith("repo:")) return kind.slice("repo:".length);
  return kind;
}

/** Who created a routine: the agent's name, "you" for the caller, else the owner. */
export function createdByLabel(row: RoutineRow): string {
  const author = row.created_by;
  if (author?.kind === "agent") return author.name;
  return row.mine ? "you" : row.owner_display;
}

/** How a routine runs: a recipe each Mac runs, or (the default) a shared job. */
export function runsOn(def: Pick<RoutineDef, "runs_on">): RoutineRunsOn {
  return def.runs_on === "each_mac" ? "each_mac" : "one_mac";
}

/** Where a shared job runs, as the end of its mode line. A Mac is only a host
 *  id, so another one is "another Mac", never a person's. */
function placeLabel(row: RoutineRow): string {
  if (row.armed_here) return "on this Mac";
  if (row.def.armed_host.trim() === "") return "not running anywhere";
  return "on another Mac";
}

/** Where a routine runs. A team routine always says its mode: "Runs once for
 *  the team · on this Mac / on another Mac", or "Each teammate's Mac · on here
 *  / off here". A personal one says so only when that is not this Mac; one
 *  written by hand may name no Mac, and then it runs on none until one is
 *  picked. */
export function machineLabel(row: RoutineRow): string | null {
  if (row.scope === "team") {
    if (runsOn(row.def) === "each_mac") {
      return `Each teammate's Mac · ${row.enabled ? "on here" : "off here"}`;
    }
    return `Runs once for the team · ${placeLabel(row)}`;
  }
  if (row.armed_here) return null;
  return row.def.armed_host.trim() === "" ? "Not running anywhere" : "Runs on another Mac";
}

/** What this Mac's switch on a row is called: "Run on my Mac" for a recipe
 *  each Mac runs, else the routine's enabled state. */
export function switchLabel(row: RoutineRow): string {
  return runsOn(row.def) === "each_mac" ? "Run on my Mac" : `${row.def.name} enabled`;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

function span(ms: number): string {
  if (ms < MINUTE) return "<1m";
  if (ms < HOUR) return `${Math.floor(ms / MINUTE)}m`;
  if (ms < DAY) return `${Math.floor(ms / HOUR)}h`;
  return `${Math.floor(ms / DAY)}d`;
}

/** `at` relative to `now`: "in 3h" ahead, "3h ago" behind. */
export function relativeTime(at: number, now: number): string {
  const delta = at - now;
  return delta >= 0 ? `in ${span(delta)}` : `${span(-delta)} ago`;
}

/** How long ago a past moment was; "just now" within the minute, including one
 *  the render clock has not caught up to yet. */
export function timeAgo(at: number, now: number): string {
  const delta = now - at;
  return delta < MINUTE ? "just now" : `${span(delta)} ago`;
}

/** The last run's status word. */
export function phaseLabel(phase: RoutineRunPhase): string {
  switch (phase) {
    case "queued":
      return "Queued";
    case "running":
      return "Working";
    case "succeeded":
      return "Succeeded";
    case "failed":
      return "Error";
    case "needs_you":
      return "Needs you";
    case "ended":
      return "Ended";
    case "timed_out":
      return "Timed out";
    case "cancelled":
      return "Cancelled";
    case "skipped":
      return "Skipped";
    case "coalesced":
      return "Merged";
    case "delivered":
      return "Delivered";
    case "undelivered":
      return "Undelivered";
    case "unknown":
      return "Unknown";
  }
}

/** The dot colour for a run's phase. */
export function phaseTone(phase: RoutineRunPhase): "ok" | "bad" | "busy" | "attention" | "quiet" {
  switch (phase) {
    case "succeeded":
    case "delivered":
      return "ok";
    case "failed":
    case "timed_out":
    case "undelivered":
      return "bad";
    case "queued":
    case "running":
      return "busy";
    case "needs_you":
    case "unknown":
      return "attention";
    default:
      return "quiet";
  }
}

const REASON_COPY: Record<string, string> = {
  already_running: "Skipped — previous run still running",
  target_unavailable: "Skipped — workspace unavailable",
  loop_guard: "Skipped — loop guard",
  paused: "Skipped — paused",
  off_here: "Skipped — off on this Mac. Turn on for this Mac to run here",
  queue_full: "Skipped — queue full",
  held: "Waiting — behind the run still going",
  interrupted: "Interrupted — CodeSense quit mid-run",
  exited_with_error: "Failed — the agent exited with an error",
  revision_missing: "Failed — the routine's revision is missing",
  account_mismatch: "Failed — fired for a different account",
  agent_unavailable: "Failed — the routine's agent is unavailable",
  task_unrenderable: "Failed — the task could not be filled in",
  launch_failed: "Failed — the agent did not launch",
};

/** Why a run did not run, or how it ended, as human copy; null when the phase
 *  says it all. A merged run's copy is rendered with its link by the caller. */
export function reasonCopy(
  run: Pick<RoutineRun, "phase" | "reason"> &
    Partial<Pick<RoutineRun, "action" | "detail" | "exit_code">>,
): string | null {
  const { reason } = run;
  const unresolved = /^unresolved:(.+)$/.exec(run.detail ?? "");
  if (unresolved) return `Failed — {{${unresolved[1]}}} had no value`;
  const exit = /^exit:(.+)$/.exec(reason ?? "");
  if (exit) return `The command exited with code ${exit[1]}.`;
  if (run.action === "command" && run.phase === "cancelled") return "Stopped by you";
  if (run.action === "command" && reason === "launch_failed") return "The command did not start.";
  if (reason === null) return null;
  const missed = /^missed:(\d+)$/.exec(reason);
  if (missed) {
    return `Skipped — ${missed[1]} missed while CodeSense was closed or asleep`;
  }
  return REASON_COPY[reason] ?? `${phaseLabel(run.phase)} — ${reason}`;
}

/** What a list row's status says about the routine's latest run. */
export function lastRunSummary(run: RoutineLastRun): string {
  return reasonCopy(run) ?? phaseLabel(run.phase);
}

export function commandRunLine(run: RoutineRun, now: number): string {
  if (run.phase === "failed" && run.detail) return run.detail;
  if (run.phase === "timed_out")
    return `Stopped at the ${(run.max_runtime_ms ?? 300000) / 60000}m max runtime`;
  if (run.phase === "cancelled") return "Stopped by you";
  if (run.phase === "running")
    return `Running for ${Math.max(0, Math.floor((now - (run.started_at ?? now)) / 1000))}s`;
  if (run.exit_code !== undefined && run.exit_code !== null)
    return `Exit ${run.exit_code} · ${((run.duration_ms ?? 0) / 1000).toFixed(1)}s`;
  return reasonCopy(run) ?? "";
}
