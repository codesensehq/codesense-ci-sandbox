// SPDX-License-Identifier: MIT
import { useEffect } from "react";
import { useOwnsScope } from "@/shared/lib/warm-scopes";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { create } from "zustand";

import { teamLifecycleVersion, useViewerStore } from "@/shared/stores/viewer-store";
import { registerMachineLookup } from "@/features/machines/lib/machine-offline";
import { parseTeamKey, teamKeyHasTeam } from "@/shared/lib/team-key";
import {
  LOCAL_MACHINE_ID,
  listMachines,
  type MachineScope,
  type MachineSummary,
} from "@/tauri/commands";
import { MACHINES_CHANGED_EVENT, type MachinesChangedPayload } from "@/tauri/events";

import {
  currentPrivateAuthorityEpoch,
  onPrivateAuthorityRetired,
} from "@/shared/lib/private-terminal-authority";

import { forgetMachineBuffers } from "../lib/remote-buffers";
import { forgetMachineMemory } from "../lib/machine-memory";
import { forgetMachineAppearance } from "../lib/machine-appearance";

/**
 * The machine registry per team (This Mac, the team's shared
 * machines, the user's personal ones), plus session-only acknowledgements ("Keep
 * read-only", dismissed restart notices). Where new work runs is chosen per launch
 * (the new-terminal chooser, the agent launch sheet); each tab keeps its exact origin.
 *
 * Keyed by the full teamKey (`org:team`) — a team name is only unique within its
 * org. A team's entry stays absent until a load succeeds: while it fails, no
 * Machines surface renders.
 */
interface MachinesState {
  byTeam: Record<string, MachineSummary[]>;
  failedTeams: string[];
  authorityRevision: number;
  keptReadOnly: string[];
  /** `machineId:epoch:workspace` restart notices the user already acted on. */
  dismissedEpochs: string[];
  /** Upsert one machine's post-action summary into every team list holding it
   *  (personal machines appear under every team). */
  apply: (summary: MachineSummary) => void;
  keepReadOnly: (machineId: string) => void;
  clearReadOnly: (machineId: string) => void;
  dismissEpoch: (key: string) => void;
}

/** Inventory can contain declarations outside the requested team. Scope is
 * checked at ingestion and again at the runtime projection boundary. */
export function machineVisibleToTeam(machine: MachineSummary, teamKey: string): boolean {
  if (machine.kind === "local") return machine.machineId === LOCAL_MACHINE_ID;
  if (machine.scope?.kind === "personal") return true;
  const team = parseTeamKey(teamKey);
  return (
    machine.scope?.kind === "team" &&
    machine.scope.orgSlug === team.orgSlug &&
    machine.scope.team === team.teamName
  );
}

export const useMachinesStore = create<MachinesState>((set) => ({
  byTeam: {},
  failedTeams: [],
  authorityRevision: 0,
  keptReadOnly: [],
  dismissedEpochs: [],
  apply: (summary) =>
    set((s) => {
      const byTeam: Record<string, MachineSummary[]> = {};
      for (const [teamKey, list] of Object.entries(s.byTeam)) {
        const idx = list.findIndex((m) => m.machineId === summary.machineId);
        // A declaration change also supersedes reads of its previous scope.
        if (idx !== -1) latest.delete(teamKey);
        byTeam[teamKey] =
          idx === -1
            ? list
            : machineVisibleToTeam(summary, teamKey)
              ? list.map((m, i) => (i === idx ? summary : m))
              : list.filter((m) => m.machineId !== summary.machineId);
      }
      return { byTeam };
    }),
  keepReadOnly: (machineId) =>
    set((s) =>
      s.keptReadOnly.includes(machineId) ? s : { keptReadOnly: [...s.keptReadOnly, machineId] },
    ),
  clearReadOnly: (machineId) =>
    set((s) =>
      s.keptReadOnly.includes(machineId)
        ? { keptReadOnly: s.keptReadOnly.filter((id) => id !== machineId) }
        : s,
    ),
  dismissEpoch: (key) =>
    set((s) =>
      s.dismissedEpochs.includes(key) ? s : { dismissedEpochs: [...s.dismissedEpochs, key] },
    ),
}));

/** Every machine id any loaded team list holds. */
function knownMachineIds(byTeam: Record<string, MachineSummary[]>): Set<string> {
  return new Set(Object.values(byTeam).flatMap((list) => list.map((m) => m.machineId)));
}

/** How long a machine must stay missing from every successful read before
 *  the look the user gave it is forgotten: a private entry the backend
 *  refused, or a shared declaration that was briefly unreadable, drops out of
 *  one read and comes back on the next. */
export const APPEARANCE_FORGET_AFTER_MS = 5 * 60_000;
/** When each machine was first missing from a successful read. */
const missingSince = new Map<string, number>();
/** Machines a registry change named (an agent's or this app's remove /
 *  unshare): missing from the next read means removed, so forgotten at once. */
const changedMachines = new Set<string>();

/** Note that the registry reported a change to `machineId`. */
export function noteMachineChanged(machineId: string): void {
  changedMachines.add(machineId);
}

/** Forget the look of machines that are surely gone: named by a registry
 *  change and now missing, or missing from successful reads at least
 *  {@link APPEARANCE_FORGET_AFTER_MS} apart. A machine seen again is kept. */
function settleMachineAppearances(before: Set<string>, after: Set<string>, now = Date.now()) {
  for (const id of before) {
    if (after.has(id) || missingSince.has(id)) continue;
    if (changedMachines.has(id)) {
      changedMachines.delete(id);
      forgetMachineAppearance(id);
    } else missingSince.set(id, now);
  }
  for (const [id, since] of missingSince) {
    if (after.has(id)) missingSince.delete(id);
    else if (now - since >= APPEARANCE_FORGET_AFTER_MS) {
      missingSince.delete(id);
      forgetMachineAppearance(id);
    }
  }
  for (const id of changedMachines) if (after.has(id)) changedMachines.delete(id);
}

/** Latest request per team: every mount of a Machines surface shares one read,
 *  and an older read resolving late never overwrites a newer one. */
const latest = new Map<string, Promise<boolean>>();
const teamVersions = new Map<string, number>();
/** Reads not settled yet (`latest` keeps a team's last read after it settles). */
const inFlight = new WeakSet<Promise<boolean>>();

/** Revoke cached team visibility and every outstanding read before notifying consumers. */
export function invalidateTeamMachines(teamKey: string): void {
  latest.delete(teamKey);
  teamVersions.delete(teamKey);
  useMachinesStore.setState((s) => {
    const byTeam = { ...s.byTeam };
    delete byTeam[teamKey];
    return { byTeam };
  });
}

onPrivateAuthorityRetired(() => {
  latest.clear();
  teamVersions.clear();
  for (const id of knownMachineIds(useMachinesStore.getState().byTeam)) forgetMachineBuffers(id);
  useMachinesStore.setState((s) => ({
    byTeam: {},
    failedTeams: [],
    keptReadOnly: [],
    dismissedEpochs: [],
    authorityRevision: s.authorityRevision + 1,
  }));
});

registerMachineLookup((machineId) => {
  for (const list of Object.values(useMachinesStore.getState().byTeam)) {
    const machine = list.find((m) => m.machineId === machineId);
    if (machine) return machine;
  }
  return null;
});

useViewerStore.subscribe(() => {
  for (const [teamKey, version] of teamVersions) {
    if (version !== teamLifecycleVersion(teamKey)) invalidateTeamMachines(teamKey);
  }
});

/** (Re)read one team's registry; resolves whether the read succeeded. A failure
 *  revokes cached visibility and lets the next mount try again. No team (the
 *  first-run floor) → nothing to read. */
export function loadTeamMachines(teamKey: string): Promise<boolean> {
  if (!teamKeyHasTeam(teamKey)) return Promise.resolve(false);
  const { orgSlug, teamName } = parseTeamKey(teamKey);
  const version = teamLifecycleVersion(teamKey);
  const authority = currentPrivateAuthorityEpoch();
  teamVersions.set(teamKey, version);
  const request: Promise<boolean> = listMachines(orgSlug, teamName).then(
    (machines) => {
      if (
        latest.get(teamKey) !== request ||
        version !== teamLifecycleVersion(teamKey) ||
        authority !== currentPrivateAuthorityEpoch()
      )
        return false;
      const before = knownMachineIds(useMachinesStore.getState().byTeam);
      useMachinesStore.setState((s) => ({
        byTeam: {
          ...s.byTeam,
          [teamKey]: machines.filter((machine) => machineVisibleToTeam(machine, teamKey)),
        },
        failedTeams: s.failedTeams.filter((key) => key !== teamKey),
      }));
      // A machine that left every list (removed or unshared — here, by an
      // agent or by a teammate) takes the file contents read from it along;
      // the look the user gave it goes once it is surely gone.
      const after = knownMachineIds(useMachinesStore.getState().byTeam);
      for (const id of before) {
        if (after.has(id)) continue;
        forgetMachineBuffers(id);
        forgetMachineMemory(id);
      }
      settleMachineAppearances(before, after);
      return true;
    },
    () => {
      if (latest.get(teamKey) === request) {
        invalidateTeamMachines(teamKey);
        useMachinesStore.setState((s) => ({
          failedTeams: [...new Set([...s.failedTeams, teamKey])],
        }));
      }
      return false;
    },
  );
  latest.set(teamKey, request);
  inFlight.add(request);
  void request.then(() => inFlight.delete(request));
  return request;
}

/** Re-read `teamKey` and every other team already loaded — personal machines
 *  appear under all of them, so a change (add / hide / remove) shows everywhere. */
export function reloadMachines(teamKey: string): Promise<boolean> {
  const keys = new Set([teamKey, ...Object.keys(useMachinesStore.getState().byTeam)]);
  return Promise.all([...keys].map(loadTeamMachines)).then((ok) => ok.every(Boolean));
}

/** How long a reveal waits on a read already in flight before answering
 *  from what is loaded: that read may be hung. */
export const ENSURE_MACHINES_WAIT_MS = 1500;

/** The longest a reveal waits for a machine just saved to be listed: past
 *  it, the read is taken as hung. */
export const ENSURE_MACHINE_MAX_WAIT_MS = 12_000;

/** The team's machines as of the latest read: waits for a read already in
 *  flight (one a change event started), loads a team never read, and otherwise
 *  answers from what is loaded. A hung read never holds the caller past
 *  {@link ENSURE_MACHINES_WAIT_MS} — unless `machineId` names a machine the
 *  loaded list does not hold yet (one just saved): then a read is started if
 *  none is in flight, and the answer waits for the read that can bring it
 *  (following a newer read that supersedes it), up to
 *  {@link ENSURE_MACHINE_MAX_WAIT_MS}. */
export function ensureTeamMachines(teamKey: string, machineId?: string): Promise<boolean> {
  const loaded = () => !!useMachinesStore.getState().byTeam[teamKey];
  const holdsTarget = () =>
    !machineId ||
    !!useMachinesStore.getState().byTeam[teamKey]?.some((m) => m.machineId === machineId);
  const current = latest.get(teamKey);
  const read =
    current && inFlight.has(current)
      ? current
      : !loaded() || !holdsTarget()
        ? loadTeamMachines(teamKey)
        : null;
  if (!read) return Promise.resolve(true);
  return new Promise((resolve) => {
    const done = (value: boolean) => {
      clearTimeout(usual);
      clearTimeout(cap);
      resolve(value);
    };
    const usual = setTimeout(() => {
      if (holdsTarget()) done(loaded());
    }, ENSURE_MACHINES_WAIT_MS);
    const cap = setTimeout(() => done(loaded()), ENSURE_MACHINE_MAX_WAIT_MS);
    const settle = (pending: Promise<boolean>) => {
      void pending.then((ok) => {
        const newer = latest.get(teamKey);
        if (!holdsTarget() && newer && newer !== pending) {
          settle(newer);
          return;
        }
        // A read a newer one superseded still leaves a usable list.
        done(ok || loaded());
      });
    };
    settle(read);
  });
}

/** Whether a change in `scope` can alter `teamKey`'s list. Personal machines
 *  appear under every team; an unknown layer may be any of them. */
function scopeReaches(scope: MachineScope | null, teamKey: string): boolean {
  if (!scope || scope.kind === "personal") return true;
  const team = parseTeamKey(teamKey);
  return scope.orgSlug === team.orgSlug && scope.team === team.teamName;
}

/** Refresh every loaded projection and return the backend-authoritative row. */
export async function refreshMachine(machineId: string): Promise<MachineSummary | null> {
  const keys = Object.keys(useMachinesStore.getState().byTeam);
  if (keys.length === 0 || !(await Promise.all(keys.map(loadTeamMachines))).every(Boolean)) {
    return null;
  }
  return firstMatch(useMachinesStore.getState().byTeam, machineId);
}

const EMPTY_MACHINES: MachineSummary[] = [];

/** `machines-changed` events this close together are one re-read per list. */
export const MACHINES_CHANGED_COALESCE_MS = 16;

// One subscription serves every mounted registry consumer. The backend says
// which layer changed (a registry write or a connection transition); each list
// that layer reaches is re-read in place, so open pickers and the Machines
// panel update without a remount. Event payloads grant no visibility:
// re-read through the same team and private-authority fences.
const registryConsumers = new Map<string, number>();
type RegistrySubscription = { dispose?: UnlistenFn; flush?: ReturnType<typeof setTimeout> };
let registrySubscription: RegistrySubscription | null = null;
function retainMachineRegistry(teamKey: string): () => void {
  registryConsumers.set(teamKey, (registryConsumers.get(teamKey) ?? 0) + 1);
  if (!registrySubscription) {
    const subscription: RegistrySubscription = {};
    registrySubscription = subscription;
    const refresh = (scopes: (MachineScope | null)[]) => {
      if (registrySubscription !== subscription) return;
      const keys = new Set([
        ...registryConsumers.keys(),
        ...Object.keys(useMachinesStore.getState().byTeam),
      ]);
      for (const key of keys)
        if (scopes.some((scope) => scopeReaches(scope, key))) void loadTeamMachines(key);
    };
    // One connect or save sends a burst (the write, then each connection
    // state); every list a burst reaches is re-read once.
    const queued: (MachineScope | null)[] = [];
    const enqueue = (scope: MachineScope | null) => {
      queued.push(scope);
      if (subscription.flush) return;
      subscription.flush = setTimeout(() => {
        subscription.flush = undefined;
        refresh(queued.splice(0));
      }, MACHINES_CHANGED_COALESCE_MS);
    };
    void listen<MachinesChangedPayload>(MACHINES_CHANGED_EVENT, (event) => {
      if (event.payload?.machineId) noteMachineChanged(event.payload.machineId);
      enqueue(event.payload?.scope ?? null);
    }).then(
      (dispose) => {
        if (registrySubscription !== subscription) dispose();
        else {
          subscription.dispose = dispose;
          // Always re-read after registration: an earlier snapshot can miss a
          // mutation before listen became active. Never await the previous read;
          // it may be hung. The existing read and authority fences reject it.
          refresh([null]);
        }
      },
      () => {
        /* A non-Tauri preview can still use explicit registry reads. */
      },
    );
  }
  return () => {
    const count = (registryConsumers.get(teamKey) ?? 1) - 1;
    if (count > 0) registryConsumers.set(teamKey, count);
    else registryConsumers.delete(teamKey);
    if (registryConsumers.size === 0) {
      const subscription = registrySubscription;
      registrySubscription = null;
      if (subscription?.flush) clearTimeout(subscription.flush);
      subscription?.dispose?.();
    }
  };
}

/** A team's machines — loads on first use. `null` = not available (not loaded
 *  yet, or not wired in this build). */
export function useTeamMachines(teamKey: string): MachineSummary[] | null {
  useEffect(() => retainMachineRegistry(teamKey), [teamKey]);
  const machines = useMachinesStore((s) => s.byTeam[teamKey]);
  const failed = useMachinesStore((s) => s.failedTeams.includes(teamKey));
  const authorityRevision = useMachinesStore((s) => s.authorityRevision);
  // A hidden team's host loads nothing; it reads when it is shown again.
  const owns = useOwnsScope();
  useEffect(() => {
    if (owns && !useMachinesStore.getState().byTeam[teamKey] && !latest.has(teamKey)) {
      void loadTeamMachines(teamKey);
    }
  }, [teamKey, machines, authorityRevision, owns]);
  return machines ?? (failed ? EMPTY_MACHINES : null);
}

/** Imperative read of a team's machines (null = not available). */
export function getTeamMachines(teamKey: string): MachineSummary[] | null {
  const state = useMachinesStore.getState();
  return state.byTeam[teamKey] ?? (state.failedTeams.includes(teamKey) ? EMPTY_MACHINES : null);
}

function firstMatch(
  byTeam: Record<string, MachineSummary[]>,
  machineId: string,
): MachineSummary | null {
  for (const list of Object.values(byTeam)) {
    const hit = list.find((m) => m.machineId === machineId);
    if (hit) return hit;
  }
  return null;
}

/** A machine by id, from any loaded team list (a machine is the same row under
 *  every team that sees it). Imperative — for tab titles. */
export function findMachine(machineId: string): MachineSummary | null {
  return firstMatch(useMachinesStore.getState().byTeam, machineId);
}

/** Reactive {@link findMachine} — re-renders when that machine's summary changes. */
export function useMachine(machineId: string): MachineSummary | null {
  return useMachinesStore((s) => firstMatch(s.byTeam, machineId));
}

/** Test-only: forget in-flight reads so each test starts clean. */
export function __resetMachineLoadsForTest(): void {
  latest.clear();
  teamVersions.clear();
  missingSince.clear();
  changedMachines.clear();
  useMachinesStore.setState({ failedTeams: [] });
}
