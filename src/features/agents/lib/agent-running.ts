// SPDX-License-Identifier: MIT
import { formatClock } from "@/features/inbox/model";
import type { SessionListRow } from "@/tauri/commands";

/** What a live instance of a saved agent is doing, for its Running row. */
export type InstanceState = "needs" | "running" | "unknown" | "idle";

export interface RunningInstance {
  handle: string;
  name: string;
  state: InstanceState;
  /** The state in words. */
  stateWord: string;
  /** The story, workspace or team folder it runs in. */
  where: string;
  /** A machine other than This Mac. */
  machine: string | null;
  /** When it started (`HH:MM`), when known. */
  since: string | null;
  unread: number;
}

const ORDER: Record<InstanceState, number> = { needs: 0, running: 1, unknown: 2, idle: 3 };
const WORD: Record<InstanceState, string> = {
  needs: "Needs you",
  running: "Working",
  unknown: "Not seen yet",
  idle: "Idle, finished its turn",
};

function stateOf(s: SessionListRow["state"]): InstanceState | null {
  if (s === "needs-you") return "needs";
  if (s === "running") return "running";
  if (s === "unknown") return "unknown";
  if (s === "idle") return "idle";
  return null;
}

/** Epoch ms of each session's start, by handle, from any rows that carry it (every team's). */
export function startedAtByHandle(
  rows: readonly { handle: string; startedAtMs?: number | null }[],
): Record<string, number> {
  const out: Record<string, number> = {};
  for (const r of rows) if (typeof r.startedAtMs === "number") out[r.handle] = r.startedAtMs;
  return out;
}

/** How much needs-you mail each session has sent and you have not answered, summed over the
 *  shared inbox's counts of every team. */
export function needsMailBySender(
  counts: readonly { needsYouBySender: Record<string, number> }[],
): Record<string, number> {
  const out: Record<string, number> = {};
  for (const row of counts)
    for (const [handle, n] of Object.entries(row.needsYouBySender ?? {}))
      out[handle] = (out[handle] ?? 0) + n;
  return out;
}

/**
 * Every live session, grouped by the saved agent it runs, waiting-on-you first. An exited
 * session or one whose machine is offline is not an instance. A session also waits on you when
 * it has sent needs-you mail you have not answered (`needsBySender`), even if its terminal is
 * idle; it counts once either way. `startedAt` is epoch ms by handle.
 */
export function instancesByAgent(opts: {
  sessions: readonly SessionListRow[];
  startedAt: Readonly<Record<string, number | null | undefined>>;
  needsBySender?: Readonly<Record<string, number>>;
}): Map<string, RunningInstance[]> {
  const out = new Map<string, RunningInstance[]>();
  for (const s of opts.sessions) {
    if (!s.agent) continue;
    let state = stateOf(s.state);
    if (!state) continue;
    if ((opts.needsBySender?.[s.handle] ?? 0) > 0) state = "needs";
    const started = opts.startedAt[s.handle];
    const list = out.get(s.agent) ?? [];
    list.push({
      handle: s.handle,
      name: s.displayName,
      state,
      stateWord: WORD[state],
      where: s.story ? `Story ${s.story}` : s.workspace || "Team folder",
      machine: s.machine || null,
      since: typeof started === "number" ? formatClock(started) : null,
      unread: s.unread ?? 0,
    });
    out.set(s.agent, list);
  }
  for (const list of out.values()) list.sort((a, b) => ORDER[a.state] - ORDER[b.state]);
  return out;
}

/** The live sessions of saved agent `agentKey`, waiting-on-you first. */
export function runningInstances(opts: {
  agentKey: string;
  sessions: readonly SessionListRow[];
  startedAt: Readonly<Record<string, number | null | undefined>>;
  needsBySender?: Readonly<Record<string, number>>;
}): RunningInstance[] {
  return instancesByAgent(opts).get(opts.agentKey) ?? [];
}

/**
 * The roster's sessions plus the ones the team's own read adds. A roster row that doesn't name
 * the agent it runs (an older projection) gives way to the team's read of the same handle.
 */
export function mergeInstanceSessions(
  listed: readonly SessionListRow[],
  known: readonly SessionListRow[],
): SessionListRow[] {
  const named = new Set(listed.filter((s) => s.agent).map((s) => s.handle));
  const knownHandles = new Set(known.map((k) => k.handle));
  return [
    ...listed.filter((s) => s.agent || !knownHandles.has(s.handle)),
    ...known.filter((k) => !named.has(k.handle)),
  ];
}
