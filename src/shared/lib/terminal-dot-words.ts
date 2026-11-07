// SPDX-License-Identifier: MIT
import { AGENT_STATUS_WORDS, type AgentStatus } from "./agent-status-config";
import { MARK_WORDS, markOfAgentStatus, type StatusMarkKind } from "./status-mark";
import type { TerminalAiStatus } from "./terminal-ai-status";

/**
 * A session's status mark in words — the one formatter every mark's tooltip
 * and accessible name goes through, so colour and shape are never the only
 * carriers of state. Framework-free: the facts (mark, since when, how it
 * exited) are gathered by the mark selectors; this only phrases them.
 */

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** The unit an age of `ms` is phrased in. */
function ageUnit(ms: number): { size: number; suffix: string } {
  if (ms < MINUTE) return { size: SECOND, suffix: "s" };
  if (ms < HOUR) return { size: MINUTE, suffix: "m" };
  if (ms < DAY) return { size: HOUR, suffix: "h" };
  return { size: DAY, suffix: "d" };
}

/** How long a state has held, compactly: `14s`, `2m`, `3h`, `1d` (whole units, rounded down). */
export function formatAge(ms: number): string {
  const age = Math.max(0, ms);
  const { size, suffix } = ageUnit(age);
  return `${Math.floor(age / size)}${suffix}`;
}

/** Milliseconds until {@link formatAge} of an age now `ms` next reads differently. */
export function msUntilAgeChanges(ms: number): number {
  const age = Math.max(0, ms);
  const { size } = ageUnit(age);
  return size - (age % size);
}

/** Common signal numbers (the same on macOS and Linux) → their names. */
const SIGNAL_NAMES: Readonly<Record<number, string>> = {
  1: "SIGHUP",
  2: "SIGINT",
  3: "SIGQUIT",
  6: "SIGABRT",
  9: "SIGKILL",
  11: "SIGSEGV",
  13: "SIGPIPE",
  15: "SIGTERM",
};

/** A signal as its host reported it (`SIGKILL`, or a platform text such as
 *  `Killed: 9`), named when its number is a known one. */
function signalName(signal: string): string {
  const n = /(\d+)\s*$/.exec(signal)?.[1];
  return (n !== undefined && SIGNAL_NAMES[Number(n)]) || signal;
}

/** How a process ended, in words: its signal, else `exit <code>`; null when unknown. */
export function exitWords(exit: { code: number | null; signal?: string | null }): string | null {
  if (exit.signal) return signalName(exit.signal);
  return exit.code === null ? null : `exit ${exit.code}`;
}

/**
 * A tab's own process ended, in words — the pane's ended bar, the tab's
 * marker, its accessible name and the tray all say it the same way:
 * "Crashed · SIGKILL", "Ended · exit 0", "Ended". An accessible name joins
 * with commas instead.
 */
export function formatEndedLabel(
  end: "ended" | "crashed",
  exit: { code: number | null; signal?: string | null } | null,
  separator = " · ",
): string {
  const parts = [MARK_WORDS[end]];
  const how = exit ? exitWords(exit) : null;
  if (how) parts.push(how);
  return parts.join(separator);
}

export interface MarkFacts {
  mark: StatusMarkKind;
  /** An attention result shares the question glyph but has no answer action. */
  words?: string;
  qualifier?: string;
  /** The live status behind the mark, when it adds words ("thinking", an approval). */
  status?: TerminalAiStatus;
  /** A `needs` mark waits on a question rather than an approval. */
  asking?: boolean;
  /** How long it has shown it (ms), when known. */
  ageMs?: number;
  /** A crash's or an exit's how (`exit 137`, `SIGKILL`), when known. */
  exit?: string | null;
  /** How many of its agent's subagents run now: a working mark then reads as their work. */
  subagents?: number;
}

/** Whether a `mark` reads as its agent's subagents' work: it is a working one and `subagents` is
 *  a count of them running (a whole number above zero; anything else is no count). A session
 *  that needs you, has stalled or ended says that instead. */
function showsSubagents(mark: StatusMarkKind, subagents: number | undefined): subagents is number {
  return (
    subagents !== undefined &&
    Number.isInteger(subagents) &&
    subagents > 0 &&
    (mark === "thinking" || mark === "tool")
  );
}

/** The words for a working `mark` while `subagents` of its agent's subagents run —
 *  "Working", "2 subagents" — or null when {@link showsSubagents} says it reads as its own. */
function subagentParts(mark: StatusMarkKind, subagents: number | undefined): string[] | null {
  if (!showsSubagents(mark, subagents)) return null;
  return [SUBAGENTS_WORKING_WORD, subagentCountWords(subagents)];
}

/** The status word an agent reads as while its subagents run: theirs is work too. */
export const SUBAGENTS_WORKING_WORD = "Working";

/** A count of subagents in words: "1 subagent", "3 subagents". */
export function subagentCountWords(subagents: number): string {
  return `${subagents} ${subagents === 1 ? "subagent" : "subagents"}`;
}

/** How many subagents an agent's status shows: their count while it works, else 0 (a status that
 *  needs you, has stalled or ended says that instead, as its words do). */
export function shownSubagents(status: AgentStatus, subagents: number | undefined): number {
  return showsSubagents(markOfAgentStatus(status), subagents) ? subagents : 0;
}

/** An agent's status word where a subagent mark carries their count beside it: "Working" while
 *  its subagents run (the mark says how many), else the status's own words. */
export function agentStatusWord(status: AgentStatus, subagents?: number): string {
  return shownSubagents(status, subagents) ? SUBAGENTS_WORKING_WORD : AGENT_STATUS_WORDS[status];
}

/** An agent status in words, as every status word reads it: "Working · 2 subagents"
 *  while its agent's subagents run and it is working, else the status's own words. */
export function agentStatusWords(status: AgentStatus, subagents?: number): string {
  return (
    subagentParts(markOfAgentStatus(status), subagents)?.join(" · ") ?? AGENT_STATUS_WORDS[status]
  );
}

/**
 * A mark's label: the state in words, then what qualifies it, then its age —
 * "Needs approval · 2m", "Needs answer", "Crashed · exit 137 · 5m", "Thinking",
 * "Working · 2 subagents", "Ready · 14s", "Ready". An accessible name joins them with
 * commas.
 */
export function formatMarkLabel(
  { mark, status, asking, ageMs, exit, words, qualifier, subagents }: MarkFacts,
  separator = " · ",
): string {
  // The finer mark vocabulary already distinguishes question / permission /
  // blocked / thinking / tool, so this projection only needs to disambiguate
  // the historical pending-ask edge (`asking` arrived before the dot flipped
  // to `question`) and the coarse-wire mapping of a `needs-permission` dot.
  const parts = words
    ? [words]
    : (subagentParts(mark, subagents) ??
      (mark === "question" && asking
        ? ["Needs answer"]
        : mark === "permission" && status === "needs-permission"
          ? ["Needs approval"]
          : MARK_WORDS[mark].split(" · ")));
  if ((mark === "crashed" || mark === "ended") && exit) parts.push(exit);
  if (ageMs !== undefined) parts.push(formatAge(ageMs));
  if (qualifier) parts.push(qualifier);
  return parts.join(separator);
}
