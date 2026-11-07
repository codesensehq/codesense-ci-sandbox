// SPDX-License-Identifier: MIT
import type { IdentityContext } from "../components/agent-row-identity-context";
import { resolveAgentRole, type AgentTreeNode } from "./agent-list-view";
import { agentRowNames } from "./agent-row-presentation";
import type { ListedAgentRow } from "./hand-agents";
import { isRemoteMachineId } from "@/features/machines/lib/machine-offline";
import { agentRowKey } from "@/shared/lib/agent-pins";
import { parseTeamKey } from "@/shared/lib/team-key";
import type { AgentSessionRow } from "@/tauri/commands";
import type { SessionProgress } from "@/shared/stores/viewer-store";
/** What the search reads of the pane's identity: built once per plan, not per agent. */
export type SearchIdentity = Pick<
  IdentityContext,
  | "launched"
  | "choicesOf"
  | "homeTeamKey"
  | "machineOf"
  | "machineIdOf"
  | "offlineMachineOf"
  | "routineOf"
  | "isHidden"
> & {
  /** The agent's latest activity note, as its row shows it, if any. */
  activityOf: (row: AgentSessionRow) => string | null;
};

/** An agent's searchable text: its name, task, activity, workspace (and story), team, role and
 *  machine (its name, and its id, which no row shows), "offline" and "hidden" when its row says
 *  so, then its routine, handle and CLI. */
export function searchTextOf(node: AgentTreeNode, identity: SearchIdentity): string {
  const { row } = node;
  const team = node.teamKey ?? identity.homeTeamKey;
  const role = resolveAgentRole(row, {
    launched: identity.launched.get(agentRowKey(team, row.handle)) ?? 0,
    choice: identity.choicesOf(team)[row.handle],
  });
  return [
    agentRowNames(row).full,
    row.startedWith,
    identity.activityOf(row),
    node.workspace?.label,
    node.workspace?.storyTitle,
    node.story?.storyId,
    node.story?.title,
    team && (parseTeamKey(team).teamName ?? parseTeamKey(team).orgSlug),
    role.label,
    identity.machineOf(row),
    isRemoteMachineId(identity.machineIdOf(row)) ? identity.machineIdOf(row) : null,
    // Searching "offline" finds the agents whose machine cannot be reached, and "hidden" the ones
    // whose tab is hidden, as their markers say.
    identity.offlineMachineOf(row) ? "offline" : null,
    identity.isHidden(row) ? "hidden" : null,
    identity.routineOf(row),
    row.handle,
    (row as ListedAgentRow).detected?.cli,
  ]
    .filter(Boolean)
    .join(" ");
}

/**
 * A row's latest-activity line: the trimmed progress note unless it is already the title,
 * otherwise a step-of-total hint. Null when there is nothing worth showing (no progress,
 * or only a phase, which the status dot already says). A waiting row leaves it out: its ask line
 * wins.
 */
export function activityLine(
  progress: SessionProgress | undefined,
  noteIsTitle = false,
): string | null {
  if (!progress) return null;
  const note = progress.note?.trim();
  if (note && !noteIsTitle) return note;
  // Without a separate note, keep an N-of-M hint if the agent is reporting one.
  if (
    typeof progress.step === "number" &&
    typeof progress.total === "number" &&
    progress.total > 0
  ) {
    return `${progress.step} of ${progress.total} · ${progress.phase}`;
  }
  // Phase alone is already carried by the status dot + `ag-task`; don't repeat it.
  return null;
}
