// SPDX-License-Identifier: MIT
import { openManagementTab } from "@/shared/lib/open-management-tab";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";
import { useUiStore } from "@/shared/stores/ui-store";

import { AgentsTab } from "./components/agents-tab";
import { matchAgentRow } from "./lib/agent-match";
import { useAgentsStore } from "./stores/agents-store";
import {
  chooseRoutineAgent,
  showAgent,
  showAgentCap,
  switchView,
} from "./components/agents-tab-nav";
import type { RoutineCreateSeed } from "@/features/routines/stores/routines-store";

/** The Agents tab's sentinel path. */
export const AGENTS_VIRTUAL_PATH = "virtual://agents";

let registered = false;

/** Register the Agents tab under its virtual path. Idempotent. */
export function registerAgentsTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(AGENTS_VIRTUAL_PATH, "Agents", AgentsTab);
}

/** Open (or focus) the Agents tab, with or without an active team, leaving its view as it
 *  is. `keepLayout` as in `openManagementTab`. */
export function openAgentsTab(opts?: { keepLayout?: boolean }): void {
  registerAgentsTab();
  useUiStore.getState().closeSettings();
  openManagementTab(AGENTS_VIRTUAL_PATH, opts);
}

/** Open (or focus) the Agents tab on its Agents view; with `key`, that agent is selected
 *  (behind the routine page's leave guard, from either view). An archived agent is listed
 *  only with archived ones shown, so opening one shows them. With `addRoutine`, New
 *  routine opens under the agent. */
export function openAgents(
  key?: string,
  archived = false,
  addRoutine: boolean | RoutineCreateSeed = false,
): void {
  const agents = useAgentsStore.getState();
  if (archived) agents.setIncludeArchived(true);
  if (key) showAgent(key, addRoutine);
  else if (addRoutine) chooseRoutineAgent(addRoutine === true ? undefined : addRoutine);
  else switchView("agents");
  openAgentsTab();
}

/** Open the saved definition's editor, including a definition archived while its session runs. */
export function openAgentEditor(key: string, archived = false): void {
  openAgents(key, archived);
  useAgentsStore.getState().setView("edit");
}

/**
 * Open the Agents tab at an agent's Running instances. `key` is the definition a
 * session's launch recorded; when the list knows it under another key with the same id (the
 * inbox is addressed by that id), that one is opened.
 */
export async function openAgentRunning(key: string): Promise<void> {
  registerAgentsTab();
  const store = useAgentsStore.getState();
  if (store.rows === null) await store.loadList();
  const match = matchAgentRow(useAgentsStore.getState().rows ?? [], key);
  openAgents(match?.key ?? key, match?.archived ?? false);
  useAgentsStore.getState().showRunning();
}

/**
 * Open the Agents tab on an agent's account picker (Choose an account), behind the same leave
 * guards as any other move to an agent. The shown tab opens the picker once the agent's Overview
 * shows.
 */
export function openAgentAccount(key: string): void {
  const agents = useAgentsStore.getState();
  if (agents.rows?.find((r) => r.key === key)?.archived) agents.setIncludeArchived(true);
  // The request is made only once the move is allowed, and the store drops it if the discard
  // prompt it waits behind is cancelled.
  showAgent(key, false, () => useAgentsStore.getState().requestAccountPicker(key));
  openAgentsTab();
}

/** Open the Agents tab on an agent's Edit at `section` (Runs on, for a missing model endpoint),
 *  behind the same leave guards as any other move to an agent. */
export function openAgentEditSection(key: string, section: string): void {
  showAgent(key, false, () => useAgentsStore.getState().openEditSection(key, section));
  openAgentsTab();
}

/** The existing Needs-you backlink routes to the cap block, including archived agents. */
export function openAgentCap(target: string): void {
  const [key, routineKey] = target.split("#routine:");
  const agents = useAgentsStore.getState();
  const row = agents.rows?.find((r) => r.key === key);
  if (!row || row.archived) agents.setIncludeArchived(true);
  showAgentCap(key, routineKey);
  openAgentsTab();
}
