// SPDX-License-Identifier: MIT
import type { RoutineRow } from "@/tauri/commands";
import { useRoleEditorStore } from "../stores/role-editor-store";
import {
  onRoutineClosed,
  useRoutinesStore,
  type RoutineCreateSeed,
} from "@/features/routines/stores/routines-store";

import type { AgentsTabView } from "../stores/agents-store";
import { useAgentsStore } from "../stores/agents-store";

/**
 * SW-AGR-5: moving around the Agents tab's two views. Every move that would leave an
 * open routine page asks first over an unsaved edit (the routine page is shared by both
 * views).
 */

// A routine page closed (back, archive, move): the Schedule no longer reopens it. A view
// switch closes it too, but keeps it to reopen (see `switchView`).
onRoutineClosed(() => useAgentsStore.getState().setScheduleRoutine(null));

/** Leave any routine page, then run `then`. */
function leaveRoutine(then: () => void): void {
  useRoleEditorStore.getState().requestLeave(() => {
    const routines = useRoutinesStore.getState();
    routines.requestLeave(() => {
      if (
        useRoutinesStore.getState().selectedKey !== null ||
        useRoutinesStore.getState().creating
      ) {
        useRoutinesStore.getState().closeRoutine();
      }
      useRoutinesStore.getState().cancelCreate();
      then();
    });
  });
}

/** Switch views. Each view keeps its own selection: the agent in Agents, the routine in
 *  Schedule. */
export function switchView(view: AgentsTabView): void {
  const agents = useAgentsStore.getState();
  if (agents.tabView === view) return;
  const kept = agents.scheduleRoutine;
  leaveRoutine(() => {
    useAgentsStore.getState().setTabView(view);
    useAgentsStore.getState().setScheduleRoutine(kept);
    if (view === "schedule" && kept) {
      useRoutinesStore.getState().openRoutine(kept.agentKey, kept.key);
    }
  });
}

/** Open a routine on the Schedule's right. */
export function openScheduleRoutine(agentKey: string | null, key: string): void {
  useAgentsStore.getState().setScheduleRoutine({ agentKey, key });
  useRoutinesStore.getState().openRoutine(agentKey, key);
}

/** Show an agent in the Agents view, behind the leave guard; with `addRoutine`, New
 *  routine opens under it (filled in from a seed when one is given). */
export function showAgent(
  agentKey: string,
  addRoutine: boolean | RoutineCreateSeed = false,
  /** Runs once the move is allowed (past the routine and role leave guards), with the agent
   *  being selected. */
  after?: () => void,
): void {
  const routines = useRoutinesStore.getState();
  // Architecture: while choosing an owner, the existing draft seed is retained without an open form.
  const seed =
    addRoutine ||
    (!routines.creating && !routines.routineOwner && !routines.selectedKey
      ? routines.createSeed
      : null);
  leaveRoutine(() => {
    const agents = useAgentsStore.getState();
    agents.setTabView("agents");
    agents.select(agentKey);
    after?.();
    if (seed) {
      useRoutinesStore.getState().startCreateFor(agentKey, seed === true ? undefined : seed);
    }
  });
}

/** Show the ordinary agent list first, keeping a draft until its owner is chosen. */
export function chooseRoutineAgent(seed?: RoutineCreateSeed): void {
  leaveRoutine(() => {
    useAgentsStore.getState().setTabView("agents");
    useAgentsStore.getState().select(null);
    useRoutinesStore.setState({ createSeed: seed ?? { draft: {} } });
  });
}

/** Open the shared editor on the Schedule's right, behind the existing leave guard. */
export function newScheduleRoutine(): void {
  leaveRoutine(() => {
    useAgentsStore.getState().setScheduleRoutine(null);
    useRoutinesStore.getState().startCreate();
  });
}

/** Follow a routine link using its own owner and the existing leave guard. */
export function showRoutine(row: RoutineRow): void {
  leaveRoutine(() => {
    const agents = useAgentsStore.getState();
    if (row.def.action === "command" || !row.agent_key) {
      agents.setTabView("schedule");
      openScheduleRoutine(null, row.key);
    } else {
      agents.setTabView("agents");
      agents.select(row.agent_key);
      useRoutinesStore.getState().openRoutine(row.agent_key, row.key);
    }
  });
}

/** Spend notices land on the saved definition, respecting existing leave guards. */
export function showAgentCap(agentKey: string, routineKey?: string): void {
  leaveRoutine(() => {
    const agents = useAgentsStore.getState();
    if (routineKey) {
      agents.setTabView("schedule");
      useAgentsStore.setState({ capFocusKey: `routine:${routineKey}` });
      openScheduleRoutine(agentKey, routineKey);
      return;
    }
    agents.setTabView("agents");
    useAgentsStore.setState({ capFocusKey: agentKey });
    agents.select(agentKey);
  });
}
