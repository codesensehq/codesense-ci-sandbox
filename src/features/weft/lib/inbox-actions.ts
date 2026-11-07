// SPDX-License-Identifier: MIT
import type { InboxItem } from "./inbox-model";

/**
 * SLW-TL-7 — the Inbox's need → action model (north-star §2.3). The Inbox is
 * ACTION-RICH: each item declares what it NEEDS and the board offers the matching
 * action inline, mapped automatically with NO hunting and NO manual picking. This
 * module is the pure mapping behind the row's controls; the wiring (the CLI door, the
 * reply routing, the consent path) is a hook, so the mapping stays unit-testable.
 *
 * Two disciplines are load-bearing and encoded here:
 *  - A SMALL FIXED VOCABULARY of named controls, each word ALWAYS meaning the same thing
 *    (muscle memory — §8.5/§8.12). The primary door is always the constant "Open in CLI";
 *    the question action is always the constant "Reply". No compound or morphing verb.
 *  - Housekeeping (Reprioritize, Dismiss/Snooze) lives in ONE explicit overflow — never
 *    sprouting as inline icon buttons.
 */

/** The fixed, named set of Inbox actions. Nothing outside this set reaches a row. */
export type InboxActionKind =
  | "open-in-cli"
  | "reply"
  | "approve"
  | "reject"
  | "reprioritize"
  | "dismiss";

/**
 * The CONSTANT label for each action — the same word in every state (§8.12). "Open in
 * CLI" never becomes a compound verb ("Open in CLI · Take the wheel" is banned); "Reply"
 * never becomes "Answer and continue" (§8.5) — live-vs-exited is the system's problem.
 * Plain inbox vocabulary only — no control-plane / lease / tier word (Ratify, Steer,
 * Dispatch, subclass, T2…).
 */
export const INBOX_ACTION_LABEL: Record<InboxActionKind, string> = {
  "open-in-cli": "Open in CLI",
  reply: "Reply",
  approve: "Approve",
  reject: "Reject",
  reprioritize: "Reprioritize",
  dismiss: "Dismiss/Snooze",
};

/**
 * An item is "agent-bearing" — and so earns the always-visible Open-in-CLI door — when it
 * has a live agent handle to drive OR an agent posted it (an exited/headless run whose
 * transcript-with-Reply the door still opens). A blank handle never counts.
 */
export function isAgentBearing(item: InboxItem): boolean {
  const live = item.row.liveSessionId;
  return (typeof live === "string" && live.trim() !== "") || item.row.initiator === "agent";
}

export interface InboxActions {
  /** The always-visible primary door on an agent-bearing row — constant "Open in CLI".
   *  `null` on a plain human triage row (no agent, no CLI to open). */
  primary: "open-in-cli" | null;
  /** The need-matched inline control(s), from the fixed vocabulary, keyed off the item's
   *  DECLARED need — no manual selection, no morphing verb. */
  need: InboxActionKind[];
  /** Housekeeping, always in ONE explicit overflow (never inline icon buttons). */
  overflow: InboxActionKind[];
}

/**
 * Resolve the controls a row shows from its DECLARED need, automatically:
 *  - question (`awaiting-answer`) → **Reply**
 *  - a gate (`needs-approval` / `awaiting-validation`) → **Approve** / **Reject**
 *  - stuck / needs eyes (`needs-attention`) → **Open in CLI** (the primary — take the wheel)
 *  - done / anything else → no inline need verb (the item is a glance)
 * Housekeeping is always available in the overflow, whatever the need.
 */
export function resolveInboxActions(item: InboxItem): InboxActions {
  const agentBearing = isAgentBearing(item);
  const primary: InboxActions["primary"] = agentBearing ? "open-in-cli" : null;

  let need: InboxActionKind[];
  switch (item.need) {
    case "awaiting-answer":
      // Reply is a reply to a RUN/agent — only offered on an agent-bearing row (which then
      // always carries the Open-in-CLI door, so the exited-reply degradation never advises a
      // door that isn't there). A question with no agent/run behind it has nothing to reply to.
      need = agentBearing ? ["reply"] : [];
      break;
    case "needs-approval":
    case "awaiting-validation":
      need = ["approve", "reject"];
      break;
    // `needs-attention` (stuck / needs eyes) resolves to Open in CLI, which IS the
    // primary door — no separate need verb. `done` / absent is a glance, no verb.
    case "needs-attention":
    case "done":
    case null:
    case undefined:
    default:
      need = [];
      break;
  }

  return { primary, need, overflow: ["reprioritize", "dismiss"] };
}
