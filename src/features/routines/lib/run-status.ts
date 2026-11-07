// SPDX-License-Identifier: MIT
import type { StatusMarkKind } from "@/shared/lib/status-mark";
import type { RoutineRun, RoutineRunPhase } from "@/tauri/commands";

import { phaseLabel, reasonCopy } from "./routine-display";

/**
 * SW-AGR-5 (design §4.7): one status vocabulary for every run row: the agent's
 * Routines section, the routine page's Runs, the Schedule list and the Details popover.
 *
 * A run's mark is the shared status mark the agents list and the tabs draw: the turning
 * dots while it works, the triangle when it failed or did not happen, the dashed ring when
 * nobody knows, the hollow grey square once it happened or was set aside.
 * The word always sits beside the mark.
 */

export interface RunStatusView {
  /** What the run reads as (`data-status`): its phase, or `delivered` for a woken run
   *  still open. */
  status: RoutineRunPhase;
  word: string;
  mark: StatusMarkKind;
}

/** The binding copy for an `unknown` wake (SPEC §7, R5). Never shortened. */
export const UNKNOWN_COPY = "May have been delivered. Check the terminal before re-sending.";

const MARK: Record<RoutineRunPhase, StatusMarkKind> = {
  delivered: "ended",
  succeeded: "ended",
  queued: "ready",
  running: "thinking",
  needs_you: "question",
  unknown: "unknown",
  failed: "error",
  timed_out: "error",
  undelivered: "error",
  coalesced: "ended",
  skipped: "ended",
  cancelled: "ended",
  ended: "ended",
};

/** A run's status as its dot and word show it. A wake run stays `running` after its
 *  text was typed (the woken agent still finishes it), so a delivered one reads
 *  Delivered until it ends. */
export function runStatus(run: { phase: RoutineRunPhase; delivered_at?: number }): RunStatusView {
  const status: RoutineRunPhase =
    run.phase === "running" && run.delivered_at !== undefined ? "delivered" : run.phase;
  return {
    status,
    word: phaseLabel(status),
    mark: MARK[status],
  };
}

/** Who and where a run's copy names. */
export interface RunContext {
  /** The agent's name ("Barb"). */
  agent: string;
  /** The machine the wake aimed at, as people call it ("codesense-test-1"). */
  machine: string;
  /** How long a wake waits for idle, in words ("30 min"). */
  maxWait: string;
}

/** Why a wake was not typed, in the product's words. */
function undelivered(reason: string, ctx: RunContext): string {
  switch (reason) {
    case "busy_timeout":
      return `${ctx.agent} stayed busy for ${ctx.maxWait}`;
    case "needs_preparation":
      return `The workspace on ${ctx.machine} isn't prepared yet`;
    case "machine_offline":
      return `${ctx.machine} was offline`;
    case "machine_unknown":
      return `${ctx.machine} isn't shared with this team`;
    case "session_ended":
      return `${ctx.agent}'s session ended before it was idle`;
    case "unobservable":
      return `CodeSense can't tell when ${ctx.agent} is idle`;
    case "agent_failed":
      return `${ctx.agent}'s session had stopped with an error`;
    case "archived":
      return `${ctx.agent} was archived`;
    case "server_restarted":
      return `The terminal server on ${ctx.machine} restarted`;
    case "limit_reached":
      return `${ctx.agent}'s session has had all the wakes it can take`;
    case "draft_pending":
      return `Someone was typing in ${ctx.agent}'s session the whole time`;
    case "unavailable":
      return `${ctx.machine} stopped taking wakes`;
    case "denied":
      return `${ctx.agent} runs without permission checks and doesn't allow wakes`;
    default:
      return `Undelivered — ${reason}`;
  }
}

/** "Waiting for Barb to be idle · until 9:30" (binding verbatim, design §5.3). */
export function queuedCopy(agent: string, until: string | null): string {
  return `Waiting for ${agent} to be idle${until ? ` · until ${until}` : ""}`;
}

/** The line under a run's status: why it did not happen, or what it is waiting on; null
 *  when the word says it all. Unknown is always the full binding sentence. */
export function runLine(run: Pick<RoutineRun, "phase" | "reason">, ctx: RunContext): string | null {
  switch (run.phase) {
    case "unknown":
      return UNKNOWN_COPY;
    case "undelivered":
      return run.reason ? undelivered(run.reason.split(":")[0].trim(), ctx) : null;
    case "coalesced":
      return "Merged into the wake already waiting";
    default:
      return reasonCopy(run);
  }
}

/** An unknown wake's cause in plain words, never an enum (design §4.7, Details). */
export function unknownCause(run: Pick<RoutineRun, "wake_machine" | "result_text">): string {
  if (/evict|no longer|forgot/i.test(run.result_text ?? "")) {
    return "The machine no longer remembers this delivery.";
  }
  return !run.wake_machine || run.wake_machine === "local"
    ? "The paste wasn't confirmed."
    : "The machine didn't confirm the paste.";
}

/** Undelivered reasons a second try cannot fix: the machine is gone, the routine is
 *  archived, the session hit its limit, the next fire re-tries an offline machine, or
 *  CodeSense can't tell when the session is idle. */
const NO_RETRY = new Set([
  "machine_unknown",
  "archived",
  "limit_reached",
  "machine_offline",
  // CodeSense can't tell when the session is idle: running it again ends the same way.
  "unobservable",
]);

/** Whether a run row may offer Run now. Never for `unknown`: the text may already be in
 *  the session (R5). */
export function canRetry(run: Pick<RoutineRun, "phase" | "reason">): boolean {
  if (run.phase !== "undelivered") return false;
  const reason = (run.reason ?? "").split(":")[0].trim();
  return !NO_RETRY.has(reason);
}

/** The machine a run went to: the session it woke, else the one it started its agent on
 *  (a remote fresh run, SW-AGR-2b); undefined for This Mac or not known. */
export function runMachine(run: {
  wake_machine?: string;
  launch_machine?: string;
}): string | undefined {
  return run.wake_machine ?? run.launch_machine;
}
