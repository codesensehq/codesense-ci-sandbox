// SPDX-License-Identifier: MIT
import type { AgentSessionRow, AttentionNeed } from "@/tauri/commands";
import type { NeedsYouKind } from "@/features/viewer/lib/cli-workspace/store/terminal-tab-selectors";
import type { SessionNeedingYou } from "@/features/weft/hooks/use-sessions-needing-you";

/**
 * The pure half of the "needs you" alerts: which entries are new enough to
 * notify, and the words a notification and its in-app Jump pill say. Which
 * sessions need you is the shared needs-you list (`selectSessionsNeedingYou`) plus
 * the roster's agents with no tab; the controller (`attention-alerts-controller.ts`)
 * owns the timers, the focus rule and the OS calls.
 */

/** An Activity-board item the user owes (a Weft YOU-OWE row). */
export interface ActivityAlert {
  kind: "activity";
  id: number;
  summary: string;
  /** The declared `need` the row carries (SLW-TL-4). `awaiting-answer`,
   *  `needs-approval` and `awaiting-validation` are BLOCKING — the human must
   *  act, so they notify at the same floor as a terminal `question` / `permission`
   *  / `crashed`, regardless of the opt-in Inbox toggle. `needs-attention` (and
   *  no declared need) stay subject to the opt-in. */
  need?: AttentionNeed | null;
  /** Reveal the Inbox. */
  open: () => void;
}

/** The declared needs that MUST notify (the human is blocked waiting). The opt-in
 *  Inbox toggle gates only ambient `needs-attention` posts. */
const BLOCKING_ACTIVITY_NEEDS = new Set<AttentionNeed>([
  "awaiting-answer",
  "needs-approval",
  "awaiting-validation",
]);

/** True when this activity row carries a BLOCKING declared need and must notify
 *  regardless of the opt-in Inbox toggle (the user is waiting to be told). */
export function isBlockingActivityAlert(item: AlertItem): item is ActivityAlert {
  return item.kind === "activity" && item.need != null && BLOCKING_ACTIVITY_NEEDS.has(item.need);
}

/** A terminal tab that needs you: a labelled row of the shared needs-you list. */
export type TerminalAlert = Pick<
  SessionNeedingYou,
  "sessionId" | "teamKey" | "workspaceId" | "tabId" | "hidden" | "kind" | "label" | "tabName"
> &
  Partial<Pick<SessionNeedingYou, "seen">> & {
    /** The question text, the crash's exit status, or the error's reason, when known. */
    detail?: string;
  };

/** An agent that needs you but has no tab in the terminal layout: one running on
 *  another machine that only the Agents pane lists, or one whose workspace is
 *  closed. Named, and opened, from its roster row. */
export interface AgentAlert {
  kind: NeedsYouKind;
  sessionId: string;
  /** The agent's name. */
  label: string;
  seen?: boolean;
  detail?: string;
  agent: AgentSessionRow;
}

/** An agent that finished a turn while the user was away. One per turn. */
export interface FinishedAlert {
  kind: "finished";
  sessionId: string;
  /** When the turn began: tells one turn's entry from the next. */
  turnStartedAt: number;
  durationMs: number;
  label: string;
  /** Its tab's name, when it has a tab. */
  tabName?: string;
  /** The summary it left, when it left one. */
  detail?: string;
  /** Its roster row, when it has no tab. */
  agent?: AgentSessionRow;
}

/** A session that needs you, with a tab or without one. */
export type NeedsYouAlert = TerminalAlert | AgentAlert;

export type AlertItem = NeedsYouAlert | FinishedAlert | ActivityAlert;

/** Identity of one state entry: the same session re-entering a state (or entering
 *  a different one) is a new entry, and so is each finished turn. A session that
 *  needs you has the same entry whether it is listed by its tab or its roster row. */
export function entryKey(item: AlertItem): string {
  if (item.kind === "activity") return `activity:${item.id}`;
  if (item.kind === "finished") return `${item.sessionId}:finished:${item.turnStartedAt}`;
  return `${item.sessionId}:${item.kind}`;
}

export interface AlertGate {
  /** The user's "notify when an agent needs you" setting. */
  enabled: boolean;
  /** Entries notified too recently to notify again (a flapping tab). */
  recentlyNotified?: ReadonlySet<string>;
}

/**
 * Which entries are new. `known` holds the entries already accounted for
 * (notified, offered in the app, or absorbed because notifications were off or
 * the entry notified too recently). Whether a fresh entry becomes an OS
 * notification or an in-app offer is the controller's call (it knows the focus). Returns the entries that should
 * notify now and the next `known` set, rebuilt from the current items so an
 * entry that resolves is forgotten and a later re-entry is considered again.
 */
export function planAlerts<T extends AlertItem>(
  known: ReadonlySet<string>,
  items: readonly T[],
  gate: AlertGate,
): { fresh: T[]; known: Set<string> } {
  const next = new Set<string>();
  const fresh: T[] = [];
  for (const item of items) {
    const key = entryKey(item);
    next.add(key);
    if (known.has(key)) continue;
    if (!gate.enabled || gate.recentlyNotified?.has(key)) continue;
    fresh.push(item);
  }
  return { fresh, known: next };
}

const STATE_WORDS: Record<NeedsYouKind, string> = {
  permission: "needs your permission",
  question: "has a question for you",
  blocked: "is blocked",
  crashed: "crashed",
  failed: "hit an error",
  attention: "needs your attention",
  startup: "is waiting on a startup screen",
};

const MAX_QUESTION = 80;
const MAX_SUMMARY = 80;
const MAX_SUMMARY_LINES = 3;

function clip(text: string, max: number): string {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

/** Where: "SW-1 · Tab 2" for a tab, the agent's name for one without. */
function placeOf(item: NeedsYouAlert | FinishedAlert): string {
  const tabName = "tabName" in item ? item.tabName : undefined;
  return tabName ? `${item.label} · ${tabName}` : item.label;
}

/** One line naming where and what: "SW-1 · Tab 2 needs your permission". */
export function alertLine(item: AlertItem): string {
  if (item.kind === "activity") return `Inbox: ${clip(item.summary, MAX_SUMMARY)}`;
  if (item.kind === "finished") return `${placeOf(item)} finished`;
  return `${placeOf(item)} ${STATE_WORDS[item.kind]}`;
}

function workedFor(ms: number): string {
  if (ms < 60_000) return `Worked for ${Math.round(ms / 1000)} seconds.`;
  const minutes = Math.round(ms / 60_000);
  return `Worked for ${minutes} ${minutes === 1 ? "minute" : "minutes"}.`;
}

function needCount(n: number, agentsOnly: boolean): string {
  const noun = agentsOnly ? "agent" : "item";
  return n === 1 ? `1 ${noun} needs you` : `${n} ${noun}s need you`;
}

/**
 * The notification for a batch of new entries: one entry gets its own message
 * (where, and the state in words); several collapse into one summary.
 */
export function composeNotification(batch: readonly AlertItem[]): { title: string; body: string } {
  if (batch.length === 1) {
    const item = batch[0];
    if (item.kind === "activity") return { title: "Something needs you", body: item.summary };
    const title = alertLine(item);
    if (item.kind === "finished") {
      return {
        title,
        body: item.detail ? clip(item.detail, MAX_SUMMARY) : workedFor(item.durationMs),
      };
    }
    if (item.kind === "question" && item.detail) {
      return { title, body: `“${clip(item.detail, MAX_QUESTION)}”` };
    }
    if (item.kind === "crashed") {
      return {
        title,
        body: item.detail ? `The agent crashed (${item.detail}).` : "The agent crashed.",
      };
    }
    if (item.kind === "failed") {
      return {
        title,
        body: item.detail
          ? `The agent stopped on an error: ${clip(item.detail, MAX_QUESTION)}`
          : "The agent stopped on an error.",
      };
    }
    if (item.kind === "attention") {
      return {
        title,
        body: item.detail
          ? clip(item.detail, MAX_QUESTION)
          : "The agent stopped and asked for a look.",
      };
    }
    if (item.kind === "permission") {
      return { title, body: "The agent is waiting for you to approve a step." };
    }
    if (item.kind === "blocked") {
      return {
        title,
        body: item.detail ? `Blocked: ${clip(item.detail, MAX_SUMMARY)}` : "The agent is blocked.",
      };
    }
    return { title, body: "The agent is waiting for your answer." };
  }
  const lines = batch.slice(0, MAX_SUMMARY_LINES).map(alertLine);
  const more = batch.length - MAX_SUMMARY_LINES;
  if (more > 0) lines.push(`+${more} more`);
  const needs = batch.filter((i) => i.kind !== "finished");
  const agentsOnly = needs.every((i) => i.kind !== "activity");
  return {
    title:
      needs.length === 0 ? `${batch.length} agents finished` : needCount(needs.length, agentsOnly),
    body: lines.join("\n"),
  };
}
