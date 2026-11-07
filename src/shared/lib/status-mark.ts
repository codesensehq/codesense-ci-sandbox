// SPDX-License-Identifier: MIT
import type { AgentStatus } from "./agent-status-config";
import type { NeedsYouKind, TerminalAiStatus } from "./terminal-ai-status";

/**
 * The one status-mark vocabulary. Every surface that shows what an agent or a
 * session is doing (workspace pills, terminal tab chips, agents list rows,
 * header badges) draws one of these marks, and each mark differs by SHAPE as
 * well as colour, so no state relies on hue alone. The twelve marks cover the
 * full agent-lifecycle × live-signals space with distinct glyphs:
 *
 * - `thinking`: three breathing dots. A hook-observed turn is in flight and no
 *   tool is open — "the model is reasoning".
 * - `tool`: a turning three-quarter arc. A tool call is in flight.
 * - `question`: filled dot with a "?" cut-out and a pulsing halo. An
 *   `ask_user` is open.
 * - `permission`: filled dot with a lock bar cut-out and a pulsing halo. A
 *   real `PermissionRequest` is pending.
 * - `blocked`: filled dot with two pause-bar cut-outs and a steady halo (no
 *   pulse). The agent reported `signal_progress {phase:"blocked"}`.
 * - `ready`: hollow ring. Turn finished (parked at a prompt).
 * - `stalled`: hollow ring with a central dot. No activity of any kind for
 *   the stalled threshold (see {@link STALLED_THRESHOLD_MS}).
 * - `ended`: hollow rounded square. Process exited cleanly, or a run finished.
 * - `crashed`: filled rounded square. Process died abnormally.
 * - `error`: filled triangle with a "!" cut-out. Declared or API error.
 * - `unknown`: dashed ring. Alive, but no hook or signal has ever spoken for it.
 * - `offline`: hollow ring with a diagonal strike. Host machine disconnected.
 *
 * Framework-free: the component that draws a mark lives in
 * `shared/components/status-mark.tsx`.
 */
export type StatusMarkKind =
  | "thinking"
  | "tool"
  | "question"
  | "permission"
  | "blocked"
  | "ready"
  | "stalled"
  | "ended"
  | "crashed"
  | "error"
  | "unknown"
  | "offline";

/** Every mark, most urgent first: the order roll-ups pick by and list counts in. */
export const STATUS_MARKS: readonly StatusMarkKind[] = [
  "question",
  "permission",
  "blocked",
  "crashed",
  "error",
  "tool",
  "thinking",
  "stalled",
  "ready",
  "unknown",
  "ended",
  "offline",
];

/**
 * How urgent each mark is, for a roll-up over several sessions or agents:
 * needs you (question / permission / blocked), then an error or a crash, then
 * work (tool / thinking), then stalled (concerning but quiet), then the quiet
 * marks (ready / unknown / ended). Offline remains visible alongside stalled.
 * Marks of equal rank tie-break by
 * {@link STATUS_MARKS} order.
 */
const MARK_RANK: Readonly<Record<StatusMarkKind, number>> = {
  question: 5,
  permission: 5,
  blocked: 5,
  crashed: 4,
  error: 3,
  tool: 2,
  thinking: 2,
  stalled: 1,
  ready: 0,
  unknown: 0,
  ended: 0,
  offline: 1,
};

/** The marks that say something is happening or waiting; the rest are quiet. */
export function isQuietMark(mark: StatusMarkKind | undefined): boolean {
  return mark === undefined || MARK_RANK[mark] === 0;
}

/** Whether `a` outranks `b` in a roll-up. */
function outranks(a: StatusMarkKind, b: StatusMarkKind): boolean {
  const d = MARK_RANK[a] - MARK_RANK[b];
  return d !== 0 ? d > 0 : STATUS_MARKS.indexOf(a) < STATUS_MARKS.indexOf(b);
}

/** The one roll-up rule: the most urgent mark among `marks`; undefined when none. */
export function topMark(marks: Iterable<StatusMarkKind | undefined>): StatusMarkKind | undefined {
  let top: StatusMarkKind | undefined;
  for (const m of marks) if (m !== undefined && (top === undefined || outranks(m, top))) top = m;
  return top;
}

/** Each mark's state in words, as tooltips, accessible names and chips say it. */
export const MARK_WORDS: Readonly<Record<StatusMarkKind, string>> = {
  thinking: "Thinking",
  tool: "Running a tool",
  question: "Needs answer",
  permission: "Needs approval",
  blocked: "Blocked",
  ready: "Ready",
  stalled: "Stalled",
  ended: "Ended",
  crashed: "Crashed",
  error: "Error",
  unknown: "Activity unknown",
  offline: "Machine offline",
};

/** A known end survives losing contact with the machine that hosted it. */
export function markWithAvailability(mark: StatusMarkKind, offline: boolean): StatusMarkKind {
  return offline && mark !== "ended" && mark !== "crashed" ? "offline" : mark;
}

/** A tab's or session's live status as a mark. A finished turn, and no status at
 *  all, is `ready`. */
export function markOfTerminalStatus(status: TerminalAiStatus | undefined): StatusMarkKind {
  switch (status) {
    case "running":
      return "thinking";
    case "thinking":
      return "thinking";
    case "needs-permission":
      return "permission";
    case "error":
      return "error";
    case "crashed":
      return "crashed";
    case "done":
    case undefined:
      return "ready";
  }
}

/** A listed agent's status as a mark. The sessions projection splits
 *  the Rust-authoritative `running` into the finer live-signal statuses so each
 *  surface draws one mark; this mapping is 1:1 — no surface keeps a private table.
 *  The `offline` mark is an offline-machine surface-level override, not a status —
 *  callers pass it directly to the StatusMark component. */
export function markOfAgentStatus(status: AgentStatus): StatusMarkKind {
  switch (status) {
    case "running":
      return "thinking";
    case "thinking":
      return "thinking";
    case "tool-running":
      return "tool";
    case "awaiting-answer":
      return "question";
    case "paused":
      return "permission";
    case "blocked":
      return "blocked";
    case "dropped":
      return "question";
    case "failed":
    case "errored":
      return "error";
    case "crashed":
      return "crashed";
    case "stalled":
      return "stalled";
    case "completed":
    case "done":
      return "ended";
    case "idle":
      return "ready";
    case "unknown":
      return "unknown";
  }
}

/** Why a session needs you, as a mark. */
export function markOfNeedsYouKind(kind: NeedsYouKind): StatusMarkKind {
  switch (kind) {
    case "question":
      return "question";
    case "permission":
      return "permission";
    case "blocked":
      return "blocked";
    case "attention":
    case "startup":
      return "question";
    case "crashed":
      return "crashed";
    case "failed":
      return "error";
  }
}

/** How a count of members in each mark reads in a roll-up ("2 need you"). */
const COUNT_WORDS: Readonly<Record<StatusMarkKind, [one: string, many: string]>> = {
  thinking: ["thinking", "thinking"],
  tool: ["running a tool", "running a tool"],
  question: ["needs you", "need you"],
  permission: ["needs approval", "need approval"],
  blocked: ["blocked", "blocked"],
  ready: ["ready", "ready"],
  stalled: ["stalled", "stalled"],
  ended: ["ended", "ended"],
  crashed: ["crashed", "crashed"],
  error: ["error", "errors"],
  unknown: ["unknown", "unknown"],
  offline: ["offline", "offline"],
};

/** How many members show each mark. */
export type MarkCounts = Partial<Record<StatusMarkKind, number>>;

/** Count the marks of a roll-up's members (members with no mark are not counted). */
export function countMarks(marks: Iterable<StatusMarkKind | undefined>): MarkCounts {
  const counts: MarkCounts = {};
  for (const m of marks) if (m !== undefined) counts[m] = (counts[m] ?? 0) + 1;
  return counts;
}

/**
 * A roll-up in words, most urgent first: "2 need you · 1 running a tool · 3 ready".
 * Quiet marks are left out when anything louder is present, so the words stay
 * about what matters; an all-quiet roll-up names its quiet states.
 */
export function rollupWords(counts: MarkCounts, separator = " · ", includeQuiet = false): string {
  const present = STATUS_MARKS.filter((m) => (counts[m] ?? 0) > 0);
  const loud = present.filter((m) => !isQuietMark(m));
  return (!includeQuiet && loud.length > 0 ? loud : present)
    .map((m) => {
      const n = counts[m]!;
      return `${n} ${COUNT_WORDS[m][n === 1 ? 0 : 1]}`;
    })
    .join(separator);
}

/** The colour a mark's words take beside it (a chip's text and tint): its own hue. */
export const MARK_TONE: Readonly<Record<StatusMarkKind, string>> = {
  thinking: "var(--cs-status-busy)",
  tool: "var(--cs-status-busy)",
  question: "var(--cs-status-needs)",
  permission: "var(--cs-status-needs)",
  blocked: "var(--cs-status-needs)",
  error: "var(--cs-status-failed)",
  crashed: "var(--cs-status-failed)",
  ready: "var(--cs-status-idle)",
  stalled: "var(--cs-status-idle)",
  unknown: "var(--cs-status-idle)",
  ended: "var(--cs-status-idle)",
  offline: "var(--cs-status-idle)",
};
