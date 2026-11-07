// SPDX-License-Identifier: MIT
/** The displayed session-status vocabulary. Rust owns the value; surfaces own its words and mark. */
export type AgentStatus =
  | "running"
  | "thinking"
  | "tool-running"
  | "paused"
  | "blocked"
  | "awaiting-answer"
  | "idle"
  | "stalled"
  | "unknown"
  | "completed"
  | "dropped"
  | "failed"
  | "crashed"
  | "done"
  | "errored";

/** Every status, in display order — the single source the contract test and any
 *  legend iterate. */
export const AGENT_STATUSES: readonly AgentStatus[] = [
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
];

/** The statuses that wait on a person: a question to answer, an approval to give,
 *  a self-reported `blocked`, an attested `signal_done(needs-attention)`, or an
 *  agent stuck on an error or a crash. The same set the tabs call "needs you"
 *  (`NeedsYouKind`), plus the managed-run overlays and the reserved-but-unbound
 *  leak. */
export const NEEDS_YOU_STATUSES: readonly AgentStatus[] = [
  "awaiting-answer",
  "paused",
  "blocked",
  "dropped",
  "failed",
  "crashed",
  "errored",
];

/** The statuses of an agent at work. */
export const WORKING_STATUSES: readonly AgentStatus[] = ["running", "thinking", "tool-running"];

/** The statuses of an agent that is neither working nor waiting on a person: parked
 *  at a prompt, finished, stalled or alive with no way to tell what it is doing.
 *  Together with {@link NEEDS_YOU_STATUSES} and {@link WORKING_STATUSES} this
 *  covers every status exactly once. */
export const IDLE_STATUSES: readonly AgentStatus[] = [
  "idle",
  "stalled",
  "unknown",
  "completed",
  "done",
];

/** The statuses whose "waiting on a person" line replaces the row's task label
 *  (`awaiting-answer`'s question, `paused`'s approval, `blocked`'s note); `dropped`
 *  is a needs-you status but has no waiting message, so its chip label stays on the
 *  row. */
const WAITING_LINE_STATUSES: readonly AgentStatus[] = ["awaiting-answer", "paused", "blocked"];

/** Whether an agent in this status is waiting on a person. */
export function isNeedsYouStatus(status: AgentStatus): boolean {
  return NEEDS_YOU_STATUSES.includes(status);
}

/** Whether this status replaces the row's task line with its own waiting message
 *  (a question, an approval prompt, a blocked note). `dropped` is needs-you without
 *  a waiting line. */
export function hasWaitingLine(status: AgentStatus): boolean {
  return WAITING_LINE_STATUSES.includes(status);
}

/** SW-BA-5 — the subset the trusted-side Rust `AgentStatus` enum mirrors and
 *  `derive_status` can emit. The FE `AgentStatus` union is a SUPERSET (adds the
 *  outcome-overlay states and the live-signal splits); the contract test asserts
 *  this ⊆ relationship rather than identity. */
export const RUST_AGENT_STATUSES = [
  "running",
  "paused",
  "done",
  "errored",
] as const satisfies readonly AgentStatus[];

/** The base states the trusted side actually serializes onto a row (`SessionRow.status`) —
 *  the honest wire type. `completed`/`dropped` are FE-derived and never cross the boundary. */
export type RustAgentStatus = (typeof RUST_AGENT_STATUSES)[number];

/**
 * Each status in words: the chip's label, the row's tooltip and the mark's
 * accessible name. One verb for an agent at work ("Working") covers the three
 * working variants' chips; the row's live mark still splits thinking vs
 * tool-running by shape.
 */
export const AGENT_STATUS_WORDS: Readonly<Record<AgentStatus, string>> = {
  running: "Working",
  thinking: "Thinking",
  "tool-running": "Running a tool",
  paused: "Needs approval",
  blocked: "Blocked",
  "awaiting-answer": "Needs answer",
  idle: "Ready",
  stalled: "Stalled",
  unknown: "Activity unknown",
  completed: "Ended",
  dropped: "Needs you",
  failed: "Error",
  crashed: "Crashed",
  done: "Ended",
  errored: "Error",
};

/**
 * SW-AV-7 — the ONE "reach it" verb, published from the same shared home as the
 * status vocabulary so Activity and Fleet cannot drift. This is the verb for the
 * single action "reach / go to the agent behind this row": the Activity `→ go`
 * chevron AND the parked/route primary verb use it, and the Fleet status-chip
 * navigation control ("{REACH_IT_VERB} to {agent}") uses it too. It kills the
 * prior go/Open/Steer synonym drift (REVIEW-SYNTHESIS resolution 17) so muscle
 * memory forms: the same word always means "reach the agent", everywhere.
 */
export const REACH_IT_VERB = "Go" as const;

/**
 * SW-AV-7 — the decision-verb vocabulary: DISTINCT verbs reserved for DISTINCT
 * decisions, never synonyms for {@link REACH_IT_VERB}. Each surface draws its
 * primary-verb label from THIS single table so a decision reads identically
 * wherever it appears. Maps to the design's §4.3 vocab table.
 *
 * `steer` is deliberately kept distinct from reach-it: on a live agent it both
 * reaches the worker AND opens the inline composer to message it (SW-AV-9) — a
 * reach-AND-message, not a bare reach. `ack`/`review` cover the DONE-UNACKED
 * decision (the code ships `review` — SW-WV-F4's review-at-altitude opener; the
 * design names it `ack`); both are registered so neither drifts into a synonym.
 */
export const DECISION_VERBS = {
  ratify: "Ratify",
  approve: "Approve",
  answer: "Answer",
  ack: "Ack",
  review: "Review",
  steer: "Steer",
} as const;

export type DecisionVerb = (typeof DECISION_VERBS)[keyof typeof DECISION_VERBS];
