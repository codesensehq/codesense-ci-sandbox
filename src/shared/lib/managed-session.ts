// SPDX-License-Identifier: MIT
/**
 * SW-AW-4 — the managed task-session supervision contract, shared types.
 *
 * These shapes are AGENT-AGNOSTIC and SESSION-KEYED (AC-11): any workspace
 * agent binds the same types. They live in `src/shared/lib/` so the managed-
 * session store, the `ask_user` reply bridge, and the chrome can all reuse
 * them without a cross-feature import. Carry NO `"claude"` or other
 * agent-specific literals.
 *
 * The `AskUser*` shapes mirror MCP elicitation's three-action model
 * (`accept`/`decline`/`cancel`) so a future native-elicitation client is a thin
 * adapter, even though v1 rides our own emit→UI→oneshot transport.
 */

/**
 * The task-level lifecycle of one managed agent session. Distinct from
 * `aiStatus` (turn/agent activity) and process-tree liveness (idle ≠ done):
 * this is the SUPERVISION state the human reads at a glance.
 *
 * - `running` — working autonomously; the net (idle/Stop→needs-attention) is armed.
 * - `awaiting-answer` — blocked on an `ask_user`; idle here is EXPECTED, so the
 *   net is suppressed (AC-6 interlock a).
 * - `done` — the agent called `signal_done` (the only confident finish), or a
 *   single-turn recipe's `Stop` was promoted.
 * - `failed` — the agent explicitly called `signal_done(failed)`. This attested
 *   terminal failure is distinct from an uncertain stop or crash.
 * - `needs-attention` — stopped/errored/timed-out WITHOUT `signal_done`; goes
 *   straight to the human (never a silent "done", never an infinite spinner).
 */
export type ManagedSessionState =
  | "running"
  | "awaiting-answer"
  | "done"
  | "failed"
  | "needs-attention";

/**
 * Terminal states — the run has FINISHED; no automatic idle transition, and the run
 * is "parked" for supervision (the reuse/reap gate + `sendToSession` refusal key on
 * this, see `isBoundSessionParked`).
 */
export const TERMINAL_MANAGED_STATES: readonly ManagedSessionState[] = [
  "done",
  "failed",
  "needs-attention",
];

/** True when a managed session has reached a terminal state. */
export function isTerminalManagedState(state: ManagedSessionState): boolean {
  return TERMINAL_MANAGED_STATES.includes(state);
}

/**
 * `signal_done`'s status = the `attempt_completion` convention's status
 * (`success | failed | needs-attention`). Maps onto `ManagedSessionState`:
 * `success` → `done`; `failed` → `failed`; `needs-attention` →
 * `needs-attention`. Only the agent's explicit attestation enters `failed`.
 */
export type DoneStatus = "success" | "failed" | "needs-attention";

/** Resolve a `signal_done` status to the terminal managed state it produces. */
export function managedStateForDoneStatus(status: DoneStatus): ManagedSessionState {
  switch (status) {
    case "success":
      return "done";
    case "failed":
      return "failed";
    case "needs-attention":
      return "needs-attention";
  }
}

/**
 * MCP elicitation's three-action response model. Do NOT invent a bespoke cancel
 * enum — `accept`/`decline`/`cancel` already covers every no-wedge path:
 * - `accept` — the user answered; `content` holds the answer.
 * - `decline` — the user explicitly refused to answer.
 * - `cancel` — dismissed / timed-out / session-died; no choice was made.
 * - `handled` — the human settled it themselves outside the question (e.g. did
 *   the action in the pane), so the agent should not redo it.
 */
export type AskUserAction = "accept" | "decline" | "cancel" | "handled";

/** The UI's reply to a blocking `ask_user`. `content` is present only on `accept`. */
export interface AskUserResponse {
  action: AskUserAction;
  /** The answer text — present iff `action === "accept"`. */
  content?: string;
}

/**
 * A blocking question emitted by the `ask_user` MCP verb (`mcp:ask-user`).
 * `correlationId` is RUST-MINTED (the agent never supplies or sees it — it is
 * blocked on the oneshot); the FE echoes it back via `resolveAskUser` to route
 * the answer to the exact awaiting call.
 */
export interface AskUserRequest {
  /** Rust-minted routing key for the reply. */
  correlationId: string;
  /** The agent session this question belongs to (its `$CODESENSE_TAB_SESSION_ID`). */
  sessionId: string;
  /** The question text to show the human. */
  question: string;
  /** Optional discrete choices; free-text answer when absent. */
  options?: string[];
  /** v1: sorts/presents questions to the human — NEVER auto-answers (AC-4). */
  category?: string;
  /** v1: urgency/impact hint for presentation — NEVER auto-answers (AC-4). */
  stakes?: string;
}

/** Build an `accept` response carrying the user's answer. */
export function acceptAnswer(content: string): AskUserResponse {
  return { action: "accept", content };
}

/** Build a `decline` response (user explicitly refused). */
export function declineAnswer(): AskUserResponse {
  return { action: "decline" };
}

/** Build a `cancel` response (dismissed / timed-out / session-died). */
export function cancelAnswer(): AskUserResponse {
  return { action: "cancel" };
}

/** Build a `handled` response (the human settled it themselves). */
export function handledAnswer(): AskUserResponse {
  return { action: "handled" };
}

/** The pending question for one session, wherever its row is presented. */
export function questionFor(
  asks: Readonly<Record<string, { sessionId: string; question: string }>>,
  sessionId: string,
): string | undefined {
  for (const ask of Object.values(asks)) if (ask.sessionId === sessionId) return ask.question;
  return undefined;
}
