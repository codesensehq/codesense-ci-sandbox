// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";
import { listen } from "@tauri-apps/api/event";

import {
  reviewTerminalServerUpdate,
  startTerminalServersUpdate,
  terminalServersUpdateStatus,
  type MachineSummary,
  type ServerExpectation,
  type TerminalServersUpdateRun,
  type TerminalServersUpdateOutcome,
  type TerminalWorkspaceLayout,
} from "@/tauri/commands";
import { getErrorMessage } from "@/shared/lib/error-message";
import {
  machineTerminalServerUpdateRunning,
  notifyMachineTerminalServerReplaced,
  setUpdateAllRunning,
} from "@/features/shell/lib/machine-terminal-server";
import { refreshMachine } from "@/features/machines/stores/machines-store";

/**
 * SW-AGR-19 — "Update all terminal servers": every server older than this
 * app. The person reviews what ends on each; the app's backend runs the
 * update (PR-8: remote machines first, This Mac last, each only as it was
 * reviewed — the same server and lifetime, no more sessions than confirmed —
 * and This Mac not after a remote machine was left without a server). The run
 * lives in the backend: this module mirrors it for any window that shows it.
 */

/** One terminal that ends, for the confirmation. */
export interface EndingTerminal {
  /** Its workspace's layout key (names can repeat across teams). */
  workspaceKey: string;
  workspace: string;
  name: string;
  /** The agent it runs, if any. */
  agent?: string;
  /** That agent is working right now. */
  working?: boolean;
}

export interface UpdateAllEntry {
  machine: MachineSummary;
  terminals: EndingTerminal[];
}

/** The agent a tab runs, from its launch tags. */
export function agentOfTags(tags: Record<string, string>): string | undefined {
  if (tags.remoteAgentKey) return tags.remoteAgentKey;
  if (tags.launchAgentRevision) return tags.launchAgentRevision.split("@")[0];
  const launch = tags.remoteLaunch?.replace(/"/g, "");
  return launch && launch !== "shell" ? launch : undefined;
}

/** The machines to update, in order (remote first, This Mac last), each with
 *  the live terminals its layout holds. */
export function updateAllEntries(
  machines: readonly MachineSummary[],
  workspacesOf: (machineId: string) => [string, TerminalWorkspaceLayout][],
  stateOf: (session: string) => string | undefined,
): UpdateAllEntry[] {
  const older = machines.filter(
    (m) => m.status.state === "connected" && m.terminalServer?.relation === "older",
  );
  const ordered = [
    ...older.filter((m) => m.kind !== "local"),
    ...older.filter((m) => m.kind === "local"),
  ];
  return ordered.map((machine) => ({
    machine,
    terminals: workspacesOf(machine.machineId).flatMap(([workspaceKey, workspace]) =>
      workspace.tabs
        .filter((tab) => tab.state.kind === "live")
        .map((tab) => {
          const agent = agentOfTags(tab.tags);
          const state = agent ? stateOf(tab.session) : undefined;
          return {
            workspaceKey,
            workspace: workspace.descriptor.displayName,
            name: tab.name,
            ...(agent ? { agent } : {}),
            ...(state === "working" || state === "thinking" ? { working: true } : {}),
          };
        }),
    ),
  }));
}

export type UpdateAllOutcome = TerminalServersUpdateOutcome;

/** What the person reviews for one machine: what its update binds to (and
 *  the sessions it ends), or why it is not updated in this run. */
export interface UpdateAllReview {
  expected?: ServerExpectation;
  problem?: string;
}

const messageOf = (error: unknown) =>
  typeof error === "object" && error !== null && "message" in error
    ? String((error as { message: unknown }).message)
    : getErrorMessage(error);

/** Read what each machine's update would bind to, for the confirmation. */
export async function reviewUpdateAll(
  entries: readonly UpdateAllEntry[],
): Promise<Record<string, UpdateAllReview>> {
  const pairs = await Promise.all(
    entries.map(async (entry): Promise<[string, UpdateAllReview]> => {
      const id = entry.machine.machineId;
      try {
        return [id, { expected: await reviewTerminalServerUpdate(id) }];
      } catch (error) {
        return [id, { problem: messageOf(error) }];
      }
    }),
  );
  return Object.fromEntries(pairs);
}

/** The session count confirmed for `entry`: its server's own, else the live
 *  tabs its layout shows. */
export function confirmedCount(entry: UpdateAllEntry, review: UpdateAllReview | undefined): number {
  return review?.expected?.sessions ?? entry.terminals.length;
}

/** The run as this window shows it: the backend's, with the entries it was
 *  reviewed from (when this window started it). */
export interface UpdateAllRun {
  opId: string | null;
  running: boolean;
  /** The machine being updated now. */
  current: string | null;
  entries: readonly UpdateAllEntry[];
  outcomes: UpdateAllOutcome[];
}

const IDLE: UpdateAllRun = {
  opId: null,
  running: false,
  current: null,
  entries: [],
  outcomes: [],
};
let run: UpdateAllRun = IDLE;
const runListeners = new Set<() => void>();
let polling: ReturnType<typeof setTimeout> | null = null;
/** The backend's progress event for every run. */
export const UPDATE_RUN_EVENT = "terminal-servers-update";
/** How often a running backend run is read again (events may be missed). */
const POLL_MS = 400;

function setRun(next: UpdateAllRun) {
  run = next;
  setUpdateAllRunning(next.running);
  for (const listener of runListeners) listener();
}

export function useUpdateAllRun(): UpdateAllRun {
  return useSyncExternalStore(
    (listener) => {
      runListeners.add(listener);
      return () => runListeners.delete(listener);
    },
    () => run,
  );
}

/** Take in the backend's run: each newly settled machine's status is read
 *  again, and whoever waits on a replaced server is told. */
function absorb(backend: TerminalServersUpdateRun, entries: readonly UpdateAllEntry[]) {
  const settled = backend.outcomes.slice(run.opId === backend.opId ? run.outcomes.length : 0);
  for (const outcome of settled) {
    void Promise.resolve()
      .then(() => refreshMachine(outcome.machineId))
      .catch(() => null);
    if (outcome.kind === "updated") notifyMachineTerminalServerReplaced(outcome.machineId);
  }
  setRun({
    opId: backend.opId,
    running: backend.running,
    current: backend.current ?? null,
    entries,
    outcomes: backend.outcomes,
  });
}

function follow(opId: string) {
  if (polling) clearTimeout(polling);
  polling = setTimeout(() => {
    polling = null;
    void terminalServersUpdateStatus(opId)
      .then((backend) => {
        if (!backend || run.opId !== opId) return;
        absorb(backend, run.entries);
        if (backend.running) follow(opId);
      })
      .catch(() => follow(opId));
  }, POLL_MS);
}

/** Pick up a run going on in the app (started in another window, over MCP,
 *  or before this one reloaded). A finished run is never picked up again. */
export async function resumeUpdateAllRun(): Promise<void> {
  const backend = await terminalServersUpdateStatus(null).catch(() => null);
  if (!backend?.running) return;
  absorb(backend, run.opId === backend.opId ? run.entries : []);
  follow(backend.opId);
}

/** Follow the backend's runs as they go (any window, any starter): a run in
 *  progress holds single machine updates here too. Returns the unlisten. */
export async function followUpdateRuns(): Promise<() => void> {
  return listen<TerminalServersUpdateRun>(UPDATE_RUN_EVENT, (event) => {
    const backend = event.payload;
    // A run this window does not know is taken in only while it runs.
    if (run.opId !== backend.opId && !backend.running) return;
    absorb(backend, run.opId === backend.opId ? run.entries : []);
  });
}

/** Start the run in the backend (`null` when a single machine's update is
 *  open or running, or nothing is reviewed to update). An error is the
 *  backend's refusal (a run already going on). */
export async function startUpdateAll(
  entries: readonly UpdateAllEntry[],
  reviewOf: (entry: UpdateAllEntry) => UpdateAllReview | undefined,
): Promise<UpdateAllRun | null> {
  if (run.running || machineTerminalServerUpdateRunning()) return null;
  const items = entries.flatMap((entry) => {
    const expected = reviewOf(entry)?.expected;
    return expected ? [{ machineId: entry.machine.machineId, expected }] : [];
  });
  if (items.length === 0) return null;
  const backend = await startTerminalServersUpdate(items);
  absorb(backend, entries);
  if (backend.running) follow(backend.opId);
  return run;
}

/** Forget a finished run (the dialog opened afresh). */
export function clearUpdateAllRun() {
  if (!run.running) setRun(IDLE);
}

export function resetUpdateAllRunForTests() {
  if (polling) clearTimeout(polling);
  polling = null;
  run = IDLE;
  setUpdateAllRunning(false);
  runListeners.clear();
}

/** The run's outcomes, counted: "1 updated, 1 failed, 1 skipped". */
export function outcomeCounts(outcomes: readonly UpdateAllOutcome[]): string {
  const count = (kind: UpdateAllOutcome["kind"]) => outcomes.filter((o) => o.kind === kind).length;
  return (["updated", "failed", "skipped"] as const)
    .filter((kind) => count(kind) > 0)
    .map((kind) => `${count(kind)} ${kind}`)
    .join(", ");
}
