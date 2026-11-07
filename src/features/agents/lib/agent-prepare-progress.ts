// SPDX-License-Identifier: MIT
// SW-XS-7: the minimal listener for Step 2 (agent preparation) progress. XS-10
// renders it (workspace caption, details); this only subscribes and reduces the
// stream to the latest state per workspace + agent — or, for an explicit Step 3
// operation (SW-XS-8), per `operationId`, so two operations on one workspace
// never fold into each other.

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  AGENT_PREPARE_PROGRESS_EVENT,
  type AgentPrepareProgressPayload,
  type AgentPrepareSummary,
} from "@/tauri/events";

/** The latest Step 2 state of one workspace + agent. */
export interface AgentPrepareState {
  workspace: string;
  agent: string;
  /** SW-XS-8: the explicit operation this state belongs to (absent: Step 2). */
  operationId?: string;
  /** Which explicit operation (`hydrate`, `hydrateStory`, `publish`). */
  operationKind?: AgentPrepareProgressPayload["operationKind"];
  phase: AgentPrepareProgressPayload["phase"];
  files: number;
  bytes: number;
  total: number;
  /** Set once a terminal or warning event carried one. */
  note?: string;
  warning?: string;
  summary?: AgentPrepareSummary;
  /** True once `ready` / `failed` / `skipped` / `cancelled` arrived. */
  settled: boolean;
  /** Plain-language line for the settled state (cancelled reads differently
   *  from failed: nothing partial landed, and it can simply be run again). */
  headline?: string;
}

/** A Step 3 operation is keyed by its id; a launch's Step 2 by workspace + agent. */
export const prepareKey = (workspace: string, agent: string, operationId?: string) =>
  operationId ? `op\u0000${operationId}` : `${workspace}\u0000${agent}`;

/** The settled line a caption shows; a publish says what happened to team content. */
export function prepareHeadline(
  phase: AgentPrepareProgressPayload["phase"],
  kind?: AgentPrepareProgressPayload["operationKind"],
): string | undefined {
  if (kind === "publish") {
    switch (phase) {
      case "ready":
        return "Published to team content";
      case "failed":
        return "Publish failed — see what was not written";
      case "cancelled":
        return "Publish stopped — team content was not changed";
      default:
        return undefined;
    }
  }
  switch (phase) {
    case "ready":
      return "Context is ready";
    case "failed":
      return "Context could not be prepared";
    case "skipped":
      return "Nothing to prepare here";
    case "cancelled":
      return "Stopped — nothing partial was left; run it again any time";
    default:
      return undefined;
  }
}

/** Fold one event into the previous state (pure; unit-tested on the golden fixture). */
export function reducePrepareProgress(
  prior: AgentPrepareState | undefined,
  event: AgentPrepareProgressPayload,
): AgentPrepareState {
  // SW-XS-10: a settled run followed by a new one (the next launch's diff)
  // starts fresh — the last run's summary, note and warning never leak into it.
  const previous = prior?.settled && event.phase === "diff" ? undefined : prior;
  const settled =
    event.phase === "ready" ||
    event.phase === "failed" ||
    event.phase === "skipped" ||
    event.phase === "cancelled";
  return {
    workspace: event.workspace,
    agent: event.agent,
    operationId: event.operationId ?? previous?.operationId,
    operationKind: event.operationKind ?? previous?.operationKind,
    // A warning annotates; it does not move the phase.
    phase: event.phase === "warning" && previous ? previous.phase : event.phase,
    files: event.files,
    bytes: event.bytes,
    total: event.total,
    note: event.phase === "warning" ? previous?.note : (event.note ?? previous?.note),
    warning: event.phase === "warning" ? event.note : previous?.warning,
    summary: event.summary ?? previous?.summary,
    settled,
    headline: settled
      ? prepareHeadline(event.phase, event.operationKind ?? previous?.operationKind)
      : undefined,
  };
}

/** Subscribe; `onChange` gets the folded state of the operation (or the
 *  workspace + agent's Step 2) that moved. */
export function listenAgentPrepareProgress(
  onChange: (state: AgentPrepareState) => void,
): Promise<UnlistenFn> {
  const states = new Map<string, AgentPrepareState>();
  return listen<AgentPrepareProgressPayload>(AGENT_PREPARE_PROGRESS_EVENT, ({ payload }) => {
    const key = prepareKey(payload.workspace, payload.agent, payload.operationId);
    const next = reducePrepareProgress(states.get(key), payload);
    // An explicit operation's state is dropped once it settles (its id is
    // never reused); a launch keeps its workspace + agent state.
    if (next.settled && payload.operationId) states.delete(key);
    else states.set(key, next);
    onChange(next);
  });
}

/** SW-XS-11 (SPEC §11.2): the persistent note for files a run kept because they
 *  were edited in the workspace. XS-10's workspace and operation details show it
 *  next to "Discard my edits" (hydrate_worktree with overwriteModified). */
export function keptModifiedNote(
  summary?: Pick<AgentPrepareSummary, "keptModifiedCount"> | number,
): string | undefined {
  // SW-XS-10: a bare count works too (the workspace's persistent note).
  const count = typeof summary === "number" ? summary : (summary?.keptModifiedCount ?? 0);
  if (count === 0) return undefined;
  return count === 1 ? "1 file kept (modified)" : `${count} files kept (modified)`;
}
