// SPDX-License-Identifier: MIT
import type { AgentRow, AgentRuntime, RoutineRow, RoutineRun } from "@/tauri/commands";
import { runsOn } from "@/features/routines/lib/routine-display";
import { machineLabel } from "@/features/routines/lib/use-routine-machines";

/**
 * SW-AGR-5 (design §4.8, SPEC §10): the Agents tab's Schedule — every agent's routines,
 * grouped by the agent they run and then by the machine they run on, with `Unassigned`
 * first when it isn't empty. Routines are never grouped by routine.
 */

/** What the Schedule list shows: one agent's routines, or what needs attention. */
export type ScheduleFilter =
  | { kind: "agent"; agentKey: string; name: string }
  | { kind: "attention" };

/** The machine slot a routine sits under: `local`, a machine id, `any`, or `each` for a
 *  recipe each teammate's Mac runs. */
export function machineOf(row: RoutineRow): string {
  if (runsOn(row.def) === "each_mac") return "each";
  return row.def.where.machine?.trim() || "local";
}

function machineName(id: string, labels: ReadonlyMap<string, string>): string {
  return id === "each" ? "each teammate's Mac" : machineLabel(id, labels);
}

/** This Mac first, then machines by name, then "any", then each teammate's Mac. */
function machineRank(id: string): number {
  if (id === "local") return 0;
  if (id === "any") return 2;
  if (id === "each") return 3;
  return 1;
}

/** A routine whose latest run did not land (or may have) or that has no agent. */
export function needsAttention(row: RoutineRow): boolean {
  if (row.unassigned || row.duplicate) return true;
  const phase = row.last_run?.phase;
  return phase === "undelivered" || phase === "unknown";
}

/** The Schedule switch's count: Unassigned routines plus undelivered or unknown latest
 *  runs the user has not opened. Zero at rest, so the switch stays quiet. */
export function attentionCount(rows: readonly RoutineRow[], seen: ReadonlySet<string>): number {
  return rows.filter((r) => {
    if (r.unassigned || r.duplicate) return true;
    return needsAttention(r) && !!r.last_run && !seen.has(r.last_run.run_id);
  }).length;
}

export interface MachineGroup<T> {
  /** `local`, a machine id, `any`, or `each`. */
  machine: string;
  label: string;
  rows: T[];
}

export interface AgentGroup<T> {
  agentKey: string | null;
  name: string;
  runtime: AgentRuntime | null;
  /** Its team, when it is a team agent ("backend · acme"). */
  team: string | null;
  /** Routines (Upcoming) or runs (Recent) in the group. */
  count: number;
  showSpace: boolean;
  machines: MachineGroup<T>[];
}

function byNextFire(a: RoutineRow, b: RoutineRow): number {
  return (
    Number(b.enabled) - Number(a.enabled) ||
    (a.next_run_at ?? Infinity) - (b.next_run_at ?? Infinity) ||
    a.def.name.localeCompare(b.def.name, undefined, { sensitivity: "base" }) ||
    a.key.localeCompare(b.key)
  );
}

function visible(rows: readonly RoutineRow[], filter: ScheduleFilter | null): RoutineRow[] {
  if (filter?.kind === "agent") return rows.filter((r) => r.agent_key === filter.agentKey);
  if (filter?.kind === "attention") return rows.filter(needsAttention);
  return [...rows];
}

/** Rows into agent → machine groups, each list in `order`. */
function groupBy<T>(
  entries: { row: RoutineRow; item: T }[],
  agents: readonly AgentRow[],
  labels: ReadonlyMap<string, string>,
): AgentGroup<T>[] {
  const byAgent = new Map<string | null, AgentGroup<T>>();
  const commandSpaces = new Set(
    entries
      .filter(({ row }) => row.def.action === "command")
      .map(({ row }) => row.key.slice(0, row.key.lastIndexOf("/"))),
  );
  for (const { row, item } of entries) {
    const key = row.def.action === "command" ? null : row.agent_key!;
    let group = byAgent.get(key);
    if (!group) {
      const agent = agents.find((a) => a.key === key);
      group = {
        agentKey: key,
        name: key === null ? "Commands" : (agent?.name ?? row.agent_display.name),
        runtime: agent?.runtime ?? null,
        team: agent?.scope === "team" ? `${agent.team} · ${agent.org}` : null,
        count: 0,
        showSpace: key === null && commandSpaces.size > 1,
        machines: [],
      };
      byAgent.set(key, group);
    }
    const id = machineOf(row);
    let machine = group.machines.find((m) => m.machine === id);
    if (!machine) {
      machine = { machine: id, label: machineName(id, labels), rows: [] };
      group.machines.push(machine);
    }
    machine.rows.push(item);
    group.count += 1;
  }
  const groups = [...byAgent.values()];
  for (const g of groups) {
    g.machines.sort(
      (a, b) => machineRank(a.machine) - machineRank(b.machine) || a.label.localeCompare(b.label),
    );
  }
  return groups.sort(
    (a, b) =>
      Number(a.agentKey === null) - Number(b.agentKey === null) ||
      a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) ||
      (a.agentKey ?? "").localeCompare(b.agentKey ?? ""),
  );
}

/** Upcoming: each agent's routines by machine, by next fire; Unassigned apart. */
export function scheduleGroups(
  rows: readonly RoutineRow[],
  agents: readonly AgentRow[],
  labels: ReadonlyMap<string, string>,
  filter: ScheduleFilter | null,
): { unassigned: RoutineRow[]; groups: AgentGroup<RoutineRow>[] } {
  const shown = visible(rows, filter);
  const held = (r: RoutineRow) =>
    r.unassigned || r.duplicate || (r.agent_key === null && r.def.action !== "command");
  const assigned = shown.filter((r) => !held(r)).sort(byNextFire);
  return {
    unassigned: shown.filter(held).sort(byNextFire),
    groups: groupBy(
      assigned.map((row) => ({ row, item: row })),
      agents,
      labels,
    ),
  };
}

/** How far back Recent reaches. */
export const RECENT_DAYS = 7;

/** A fire that never ran: shown folded under the run it belongs to, not as a row. */
function folds(run: RoutineRun): boolean {
  return run.phase === "coalesced" || run.phase === "skipped";
}

export interface RecentItem {
  row: RoutineRow;
  run: RoutineRun;
  /** Merged or skipped fires folded under this run ("+3 merged or skipped"). */
  folded: number;
}

/** When a run happened, for ordering and the time column. */
export function runAt(run: RoutineRun): number {
  return run.ended_at ?? run.started_at ?? run.created_at;
}

/** Recent: each agent's runs of the last 7 days by machine, newest first. A merged or
 *  skipped fire folds under the run before it (the one it merged into or yielded to),
 *  else under the next one. */
export function recentGroups(
  rows: readonly RoutineRow[],
  runsByKey: Readonly<Record<string, RoutineRun[] | undefined>>,
  agents: readonly AgentRow[],
  labels: ReadonlyMap<string, string>,
  filter: ScheduleFilter | null,
  now: number,
): { groups: AgentGroup<RecentItem>[] } {
  const since = now - RECENT_DAYS * 24 * 60 * 60_000;
  const entries: { row: RoutineRow; item: RecentItem }[] = [];
  for (const row of visible(rows, filter)) {
    if ((row.agent_key === null && row.def.action !== "command") || row.unassigned) continue;
    const runs = (runsByKey[row.key] ?? [])
      .filter((r) => runAt(r) >= since)
      .sort((a, b) => runAt(b) - runAt(a));
    const items: RecentItem[] = runs
      .filter((r) => !folds(r))
      .map((run) => ({ row, run, folded: 0 }));
    for (const run of runs.filter(folds)) {
      const at = runAt(run);
      const before = items.find((i) => runAt(i.run) <= at);
      const after = [...items].reverse().find((i) => runAt(i.run) > at);
      const host = before ?? after;
      if (host) host.folded += 1;
    }
    for (const item of items) entries.push({ row, item });
  }
  entries.sort((a, b) => runAt(b.item.run) - runAt(a.item.run));
  return { groups: groupBy(entries, agents, labels) };
}
