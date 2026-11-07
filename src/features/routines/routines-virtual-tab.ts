// SPDX-License-Identifier: MIT
import {
  AGENTS_VIRTUAL_PATH,
  openAgents,
  openAgentsTab,
} from "@/features/agents/agents-virtual-tab";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { registerVirtualTab } from "@/shared/lib/virtual-viewer-registry";

import { useAgentsStore } from "@/features/agents/stores/agents-store";
import { openScheduleRoutine, switchView } from "@/features/agents/components/agents-tab-nav";

import { RoutinesRedirect } from "./components/routines-redirect";
import {
  useRoutinesStore,
  type RoutineAgentFilter,
  type RoutineCreateSeed,
} from "./stores/routines-store";

/** The Routines tab's sentinel path. */
export const ROUTINES_VIRTUAL_PATH = "virtual://routines";

let registered = false;

/**
 * `virtual://routines` is retired (SPEC §10): routines live in the Agents tab. For one
 * release the path still resolves, as a redirect: a persisted or linked Routines tab
 * opens Agents → Schedule and closes itself, so the user never sees two tabs.
 */
export function registerRoutinesTab(): void {
  if (registered) return;
  registered = true;
  registerVirtualTab(ROUTINES_VIRTUAL_PATH, "Schedule", RoutinesRedirect);
  retireRoutinesTabs();
}

const isRoutinesPath = (path: string) =>
  path === ROUTINES_VIRTUAL_PATH || path.startsWith(`${ROUTINES_VIRTUAL_PATH}?`);

/** Every team's open tabs with each Routines tab rewritten to the Agents tab in place
 *  (dropped where the team already has one), and an active Routines tab made the Agents
 *  tab; null when no team has one. */
export function retiredRoutinesTabs<T extends { path: string; name: string }>(
  openTabs: Record<string, T[]>,
  activeTab: Record<string, string>,
): {
  openTabs: Record<string, T[]>;
  activeTab: Record<string, string>;
  /** The agent a rewritten tab was filtered to (`?agent=`), if any. */
  agentKey: string | null;
} | null {
  let changed = false;
  let agentKey: string | null = null;
  const tabs: Record<string, T[]> = { ...openTabs };
  const active: Record<string, string> = { ...activeTab };
  for (const [teamKey, list] of Object.entries(openTabs)) {
    if (!list.some((t) => isRoutinesPath(t.path))) continue;
    changed = true;
    const hasAgents = list.some((t) => t.path === AGENTS_VIRTUAL_PATH);
    const next: T[] = [];
    for (const tab of list) {
      if (isRoutinesPath(tab.path)) {
        agentKey ??= new URLSearchParams(tab.path.split("?")[1] ?? "").get("agent");
      }
      if (!isRoutinesPath(tab.path)) next.push(tab);
      else if (!hasAgents && !next.some((t) => t.path === AGENTS_VIRTUAL_PATH)) {
        next.push({ ...tab, path: AGENTS_VIRTUAL_PATH, name: "Agents" });
      }
    }
    tabs[teamKey] = next;
    if (active[teamKey] && isRoutinesPath(active[teamKey])) active[teamKey] = AGENTS_VIRTUAL_PATH;
  }
  return changed ? { openTabs: tabs, activeTab: active, agentKey } : null;
}

/** Rewrite persisted Routines tabs, for every team, whenever the saved layout brings one
 *  back (it can arrive after startup): never only the team in view. */
function retireRoutinesTabs(): void {
  const apply = () => {
    const s = useViewerStore.getState();
    if (!s?.openTabs) return;
    const next = retiredRoutinesTabs(s.openTabs, s.activeTab ?? {});
    if (!next) return;
    useViewerStore.setState({ openTabs: next.openTabs, activeTab: next.activeTab });
    // It opens where the Routines tab went: Agents → Schedule, filtered as it was (§2.1).
    const agents = useAgentsStore.getState();
    agents.setTabView("schedule");
    if (next.agentKey) {
      const name =
        agents.rows?.find((a) => a.key === next.agentKey)?.name ??
        next.agentKey.slice(next.agentKey.lastIndexOf("/") + 1);
      agents.setScheduleFilter({ kind: "agent", agentKey: next.agentKey, name });
    }
  };
  // Registered while the app module loads: start once that load has finished.
  setTimeout(() => {
    apply();
    useViewerStore.subscribe((s, prev) => {
      if (s?.openTabs !== prev?.openTabs) apply();
    });
  }, 0);
}

/** Open Agents → Schedule: filtered to an agent's routines when `agentFilter` names one,
 *  every agent's otherwise. Keeps its signature, so every old caller lands there.
 *  `keepLayout` as in `openManagementTab`. */
export function openRoutines(
  agentFilter: RoutineAgentFilter | null = null,
  opts?: { keepLayout?: boolean },
): void {
  const agents = useAgentsStore.getState();
  agents.setScheduleFilter(
    agentFilter ? { kind: "agent", agentKey: agentFilter.agentKey, name: agentFilter.name } : null,
  );
  switchView("schedule");
  openAgentsTab(opts);
}

/** Open a routine on Agents → Schedule (run links, scratch). */
export async function openRoutineInSchedule(key: string): Promise<void> {
  openRoutines();
  const routines = useRoutinesStore.getState();
  if (routines.rows === null) await routines.loadList();
  const row = useRoutinesStore.getState().rows?.find((r) => r.key === key);
  if (row?.agent_key) openScheduleRoutine(row.agent_key, key);
}

/** SW-AGR-6 "Keep this…": open New routine filled in from a timer. Routines belong to
 *  agents (SPEC §10), so it opens in the Agents tab, under the seed's agent, with that
 *  agent fixed (SW-AGR-5). Without an agent, keep the seed until the person picks one. */
export function openRoutineDraft(seed: RoutineCreateSeed): void {
  // Behind the leave guard: choose an agent if needed, then open its filled draft.
  openAgents(seed.draft.agent?.trim() || undefined, false, seed);
}
