// SPDX-License-Identifier: MIT
import type { AgentRow } from "@/tauri/commands";

/**
 * How a routine names its agent. A routine writes the bare id of an agent in its own
 * space and the rendered key of any other: a personal routine may run a team agent by
 * key, while a team routine runs only its own team's agents, since a teammate's Mac may
 * not have the creator's personal space or another team. Either may run a starter
 * (`builtin/claude`, `builtin/codex`) by its key: every Mac has those.
 */

/** Where a routine lives: every new routine is personal; a team routine's space is its
 *  key without the id. */
export type RoutineSpace = { scope: "personal" } | { scope: "team"; space: string };

/** A rendered key without its last segment, the id. */
function spaceOf(key: string): string {
  return key.slice(0, key.lastIndexOf("/"));
}

/** A rendered key's last segment, the id. */
function idOf(key: string): string {
  return key.slice(key.lastIndexOf("/") + 1);
}

export function routineSpace(scope: "personal" | "team", key: string | null): RoutineSpace {
  return scope === "team" && key ? { scope: "team", space: spaceOf(key) } : { scope: "personal" };
}

/** Whether `agent` lives in the routine's own space, so the routine names it by id. */
function inSpace(agent: Pick<AgentRow, "key" | "scope">, routine: RoutineSpace): boolean {
  if (routine.scope === "personal") return agent.scope === "personal";
  return agent.scope === "team" && spaceOf(agent.key) === routine.space;
}

/** The reference a routine in `routine` writes for `agent`. */
export function agentRefFor(agent: Pick<AgentRow, "key" | "scope">, routine: RoutineSpace): string {
  return inSpace(agent, routine) ? idOf(agent.key) : agent.key;
}

/** The listed agent a routine's reference names, if any is listed. */
export function findAgent(
  rows: readonly AgentRow[],
  ref: string,
  routine: RoutineSpace,
): AgentRow | undefined {
  const raw = ref.trim();
  if (!raw) return undefined;
  if (raw.includes("/")) return rows.find((r) => r.key === raw);
  return rows.find((r) => inSpace(r, routine) && idOf(r.key) === raw);
}

/** The key of the agent routine `routineKey` names with `ref`. */
export function agentKeyFor(ref: string, routineKey: string): string {
  const raw = ref.trim();
  return raw.includes("/") ? raw : `${spaceOf(routineKey)}/${raw}`;
}

/** Whether a routine in `routine` can be offered `agent` while `teamKey` (`org/team`) is
 *  the team in view: a team routine its own team's agents, a personal routine your own
 *  agents and that team's, and either one the starters, which every Mac has. */
export function offeredTo(
  agent: Pick<AgentRow, "key" | "scope" | "org" | "team">,
  routine: RoutineSpace,
  teamKey: string | null,
): boolean {
  if (agent.scope === "builtin") return true;
  if (routine.scope === "team") return inSpace(agent, routine);
  return agent.scope === "personal" || `${agent.org}/${agent.team}` === teamKey;
}
