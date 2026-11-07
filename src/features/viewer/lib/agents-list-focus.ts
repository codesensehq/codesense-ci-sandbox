// SPDX-License-Identifier: MIT
import { useViewerStore } from "@/shared/stores/viewer-store";

/**
 * Where keyboard focus goes when the agents list folds or opens: folding it moves focus to
 * the tray's Agents control (the control that brings it back); opening it moves focus into
 * the list. The side that changes the state asks; the side that mounts takes the request.
 */
export type AgentsFocusTarget = "list" | "toggle";

const pending = new Map<string, AgentsFocusTarget>();

/** Ask for focus to land on `target` once a team's list has folded or opened. */
export function requestAgentsFocus(teamKey: string, target: AgentsFocusTarget): void {
  pending.set(teamKey, target);
}

/** Take a team's pending focus request for `target`; true when there was one. */
export function takeAgentsFocus(teamKey: string, target: AgentsFocusTarget): boolean {
  if (pending.get(teamKey) !== target) return false;
  pending.delete(teamKey);
  return true;
}

/** Fold the Agents dock; focus lands on the tray's Agents control that brings it back. */
export function hideAgentsDock(teamKey: string): void {
  requestAgentsFocus(teamKey, "toggle");
  useViewerStore.getState().setWorkspacePanelCollapsed(true);
}

/** The id of a team's agents list, for the control that shows and hides it. */
export function agentsListId(teamKey: string): string {
  return `agents-list-${teamKey.replace(/[^\w-]/g, "_")}`;
}

/** Focus into a team's built-in Agents list: its first agent row, else its first control. */
export function focusAgentsList(teamKey: string): void {
  const list = document.getElementById(agentsListId(teamKey));
  const target =
    list?.querySelector<HTMLElement>(".ag-scroll button") ??
    list?.querySelector<HTMLElement>("input, button");
  target?.focus();
}
