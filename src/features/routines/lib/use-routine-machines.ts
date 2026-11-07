// SPDX-License-Identifier: MIT
import { useMemo } from "react";

import { useTeamMachines } from "@/features/machines/stores/machines-store";
import { buildTeamKey, useActiveTeamKey } from "@/shared/lib/team-key";
import { LOCAL_MACHINE_ID, type MachineSummary } from "@/tauri/commands";

/** A machine a routine can name, as its machine slot offers it. */
export interface RoutineMachine {
  id: string;
  label: string;
  offline: boolean;
}

function offline(m: MachineSummary): boolean {
  return m.kind !== "local" && m.status.state !== "connected";
}

/**
 * The machines a routine can name (SW-AGR-5, design §4.6): This Mac first, then each
 * machine shared with the team in view, by label. Personal machines are left out: a team
 * routine runs for teammates whose Macs never see them; a personal agent's routine may
 * also name the user's own machines. `labels` names every machine the
 * list holds, so a sentence reads "on codesense-test-1" rather than an id.
 */
export function useRoutineMachines(
  team?: { org: string; team: string } | null,
  /** A personal agent's routine may also name the user's own machines. */
  personal = false,
): {
  options: RoutineMachine[];
  labels: ReadonlyMap<string, string>;
  /** The team's machines have been read: only then can a machine be "not shared". */
  loaded: boolean;
  /** The ids a routine may name (`local` first), or null before the list is read. */
  known: string[] | null;
} {
  // A team routine names its own team's machines, whichever team is in view.
  const active = useActiveTeamKey();
  const teamKey = team ? buildTeamKey(team.org, team.team) : active;
  const machines = useTeamMachines(teamKey);
  return useMemo(() => {
    const list = machines ?? [];
    const shared = list
      .filter(
        (m) =>
          m.kind !== "local" &&
          (m.scope?.kind === "team" || (personal && m.scope?.kind === "personal")),
      )
      .sort((a, b) => a.label.localeCompare(b.label));
    const options: RoutineMachine[] = [
      { id: LOCAL_MACHINE_ID, label: "This Mac", offline: false },
      ...shared.map((m) => ({ id: m.machineId, label: m.label, offline: offline(m) })),
    ];
    const labels = new Map(list.map((m) => [m.machineId, m.label] as const));
    labels.set(LOCAL_MACHINE_ID, "This Mac");
    return {
      options,
      labels,
      loaded: machines !== null,
      known: machines === null ? null : options.map((m) => m.id),
    };
  }, [machines, personal]);
}

/** A machine id as people call it. */
export function machineLabel(id: string | undefined, labels: ReadonlyMap<string, string>): string {
  if (!id || id === LOCAL_MACHINE_ID) return "This Mac";
  if (id === "any") return "any connected machine";
  return labels.get(id) ?? id;
}

/** The team a routine's machines come from: its agent's team, for a team agent. */
export function machineTeamOf(
  agent: { scope: string; org: string; team: string } | undefined,
): { org: string; team: string } | null {
  return agent?.scope === "team" ? { org: agent.org, team: agent.team } : null;
}

/** `30m` / `2h` in words ("30 min", "2 h"); the backend's default when absent. */
export function waitWords(raw: string | undefined): string {
  const m = /^(\d+)\s*([mh])$/.exec((raw ?? "30m").trim());
  if (!m) return raw ?? "30 min";
  return m[2] === "m" ? `${m[1]} min` : `${m[1]} h`;
}
