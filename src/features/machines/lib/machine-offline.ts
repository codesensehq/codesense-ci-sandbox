// SPDX-License-Identifier: MIT
import type { MachineSummary } from "@/tauri/commands";

/**
 * Machine status reads with no store behind them, so any module can ask whether a machine
 * is offline without importing the machines store (and its subscriptions). The store
 * registers its lookup when it loads; until then no machine is known, so none reads as
 * offline.
 */

/** A remote machine's offline word for a badge: "offline", or "offline since
 *  14:05" when the machine was last reached at a known time. `null` while it
 *  is connected (or connecting). */
export function machineOfflineWord(machine: MachineSummary | null): string | null {
  if (!machine) return "offline";
  const s = machine.status;
  if (
    s.state === "connected" ||
    s.state === "connecting" ||
    s.state === "handshaking" ||
    s.state === "readmitting"
  )
    return null;
  if (s.state === "unreachable" && s.lastReachedAtMs > 0) {
    const at = new Date(s.lastReachedAtMs).toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    });
    return `offline since ${at}`;
  }
  return "offline";
}

type MachineLookup = (machineId: string) => MachineSummary | null;

let lookup: MachineLookup = () => null;

/** The machines store's lookup by id, registered once when the store loads. */
export function registerMachineLookup(next: MachineLookup): void {
  lookup = next;
}

/** Whether `machineId` names a remote machine: not This Mac (`local`), and not missing. Every
 *  read of where a tab or session runs goes through it, so `local` is This Mac everywhere. */
export function isRemoteMachineId(machineId: string | null | undefined): machineId is string {
  return !!machineId && machineId !== "local";
}

/** The name of the machine `machineId` when it is offline; null for This Mac, a connected
 *  machine, or one no loaded list knows. */
export function offlineMachineName(machineId: string | null | undefined): string | null {
  if (!isRemoteMachineId(machineId)) return null;
  const machine = lookup(machineId);
  // A machine whose status has not been read can't be said to be offline.
  return machine?.status && machineOfflineWord(machine) ? machine.label : null;
}

/** What a remote tab's origin says about where it runs, as the offline reads take it. */
export interface TabOrigin {
  machineId: string;
  offlineSince?: number;
}

/** What user-facing text calls a machine no loaded list knows, rather than its raw id. */
export const UNKNOWN_MACHINE_LABEL = "a remote machine";

/** The name of the machine a remote tab runs on: its label once a list knows it. */
export function machineLabelOf(machineId: string): string {
  return lookup(machineId)?.label ?? UNKNOWN_MACHINE_LABEL;
}

/**
 * The one rule for whether a remote tab can only be shown as it last was: its machine reads
 * offline, or the machine's layout could not be read live and the tab is held frozen
 * (`offlineSince`), as its tab strip shows it, even while the machine itself reads connected.
 * The machine's name, else null (This Mac's tabs, and remote tabs read live).
 */
function frozenTabMachine(
  find: MachineLookup,
  origin: TabOrigin | null | undefined,
): string | null {
  if (!origin || !isRemoteMachineId(origin.machineId)) return null;
  const machine = find(origin.machineId);
  // A machine whose status has not been read can't be said to be offline.
  if (machine?.status && machineOfflineWord(machine)) return machine.label;
  if (origin.offlineSince === undefined) return null;
  return machine?.label ?? UNKNOWN_MACHINE_LABEL;
}

/** {@link frozenTabMachine} against the machines store's lookup. */
export function frozenTabMachineName(origin: TabOrigin | null | undefined): string | null {
  return frozenTabMachine(lookup, origin);
}

/** {@link frozenTabMachine} against `byTeam`'s machine lists, as the agents list reads it: the
 *  machine as `teamKey` lists it when that team does (one machine id can be listed by two teams,
 *  each with its own status), else as any team lists it. */
export function frozenTabMachineIn(
  byTeam: Readonly<Record<string, readonly MachineSummary[]>>,
  origin: TabOrigin | null | undefined,
  teamKey?: string,
): string | null {
  return frozenTabMachine(
    (machineId) => machineInTeams(byTeam, machineId, teamKey) ?? null,
    origin,
  );
}

/** A machine by id in `byTeam`'s lists: `teamKey`'s list first when one is named, else the first
 *  team that lists it. */
export function machineInTeams(
  byTeam: Readonly<Record<string, readonly MachineSummary[]>>,
  machineId: string,
  teamKey?: string,
): MachineSummary | undefined {
  const own = teamKey ? byTeam[teamKey]?.find((m) => m.machineId === machineId) : undefined;
  if (own) return own;
  for (const list of Object.values(byTeam)) {
    const machine = list.find((m) => m.machineId === machineId);
    if (machine) return machine;
  }
  return undefined;
}
