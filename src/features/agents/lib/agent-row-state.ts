// SPDX-License-Identifier: MIT
import { registeredCli } from "@/shared/lib/agent-runtimes";
import type { StatusMarkKind } from "@/shared/lib/status-mark";
import type { AgentAccountChoice, AgentRow, AgentRuntime } from "@/tauri/commands";

import { accountLine, accountReasonCopy, runtimeLabel } from "./agent-display";
import type { RunningInstance } from "./agent-running";

/** What a list row says about its agent, most urgent first: archived, waiting on you, can't
 *  launch, running, ready. */
export type AgentRowState = "needs" | "running" | "broken" | "archived" | "ready";

/** The status mark each state draws, from the one mark vocabulary. An archived agent has none. */
export const STATE_MARK: Record<AgentRowState, StatusMarkKind | null> = {
  needs: "question",
  running: "thinking",
  broken: "error",
  ready: "ready",
  archived: null,
};

/** The status mark each state of a live instance draws. */
export const INSTANCE_MARK: Record<RunningInstance["state"], StatusMarkKind> = {
  needs: "question",
  running: "thinking",
  unknown: "unknown",
  idle: "ready",
};

export interface AgentRowStatus {
  state: AgentRowState;
  /** Live instances of the agent. */
  live: number;
  /** Instances waiting on you. */
  needsYou: number;
  /** Whether a new launch would work. False shows Fix in place of Launch. */
  canLaunch: boolean;
  /** What a Fix mends when a launch wouldn't work: its model endpoint or its account. */
  fix: "endpoint" | "account" | null;
}

/** What a saved agent says when its CLI takes its model from an endpoint and it names none. */
export const NEEDS_MODEL_ENDPOINT = "Needs a model endpoint";

/** Whether `row`'s CLI runs only on a configured model endpoint (the registry says so) and the
 *  agent names no model, so a launch would be refused. */
export function needsModelEndpoint(row: Pick<AgentRow, "runtime" | "model">): boolean {
  return registeredCli(row.runtime)?.modelEndpoint === true && row.model === null;
}

/** The row's state from the list row and its live instances. */
export function agentRowStatus(
  row: Pick<AgentRow, "archived" | "running_now" | "account" | "runtime" | "model">,
  instances: readonly RunningInstance[],
): AgentRowStatus {
  const needsYou = instances.filter((i) => i.state === "needs").length;
  const live = Math.max(row.running_now, instances.length);
  const fix = needsModelEndpoint(row)
    ? "endpoint"
    : accountLine(row.account, row.runtime).kind === "ok"
      ? null
      : "account";
  const canLaunch = fix === null;
  const state: AgentRowState = row.archived
    ? "archived"
    : needsYou > 0
      ? "needs"
      : !canLaunch
        ? "broken"
        : live > 0
          ? "running"
          : "ready";
  return { state, live, needsYou, canLaunch, fix };
}

/** The model an agent names, or the CLI's own default. */
export function modelWord(row: Pick<AgentRow, "model">): string {
  return row.model ?? "CLI default";
}

/** Which account pays, short: the pinned one, the Active one marked as such, or your own CLI
 *  login. The names come from `accountLine`, so every surface spells them one way. */
export function shortAccount(account: AgentAccountChoice, runtime: AgentRuntime): string {
  if (account.provider === null) return "";
  const line = accountLine(account, runtime);
  return line.kind === "ok" &&
    (account.source === "binding" || account.resolved_source === "active")
    ? `on ${line.text}`
    : "your own login";
}

/** A row's second line in its parts. `cli` is the CLI's word, which a Preview badge follows;
 *  `rest` is what comes after it, and `tail` the schedule a can't-launch row still names, which
 *  gives way first when the row is narrow. A row waiting on you or archived names no CLI. */
export interface AgentRowLineParts {
  cli: string | null;
  rest: string;
  tail: string | null;
}

/** A row's second line. A row waiting on you or archived says that; one that can't launch gives
 *  the reason, and its next run that can't start; any other says what it runs on and when it
 *  runs next. */
export function agentRowLineParts(
  row: Pick<AgentRow, "runtime" | "model" | "account">,
  state: AgentRowState,
  next: string | null,
): AgentRowLineParts {
  if (state === "needs") return { cli: null, rest: "Waiting on you", tail: null };
  if (state === "archived") return { cli: null, rest: "Archived", tail: null };
  const cli = runtimeLabel(row.runtime);
  if (state === "broken") {
    const reason = needsModelEndpoint(row)
      ? NEEDS_MODEL_ENDPOINT
      : row.account.store_error
        ? "accounts couldn't be read"
        : accountReasonCopy(row.account.reason);
    return { cli, rest: reason, tail: next ? `Next ${next} can't start` : null };
  }
  const rest = [modelWord(row), shortAccount(row.account, row.runtime), next && `Next ${next}`]
    .filter(Boolean)
    .join(" · ");
  return { cli, rest, tail: null };
}

/** {@link agentRowLineParts} as one line of words. */
export function agentRowLine(
  row: Pick<AgentRow, "runtime" | "model" | "account">,
  state: AgentRowState,
  next: string | null,
): string {
  const { cli, rest, tail } = agentRowLineParts(row, state, next);
  return [cli, rest, tail].filter(Boolean).join(" · ");
}
