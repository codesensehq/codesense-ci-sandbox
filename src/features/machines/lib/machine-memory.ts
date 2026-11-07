// SPDX-License-Identifier: MIT
import { create } from "zustand";

import { useLocalProfileStore } from "@/shared/stores/local-profile-store";
import { useViewerStore } from "@/shared/stores/viewer-store";
import { parseWorkspaceKey } from "@/shared/lib/workspace-key";
import type { TerminalLayoutProjection } from "@/shared/lib/terminal-layout-projection";
import { LOCAL_MACHINE_ID, type MachineSummary } from "@/tauri/commands";

/**
 * What this app remembers about remote Machines across launches, for their
 * status line only:
 *
 * - the census each Machine held the last time it was seen live (workspace
 *   names and tab counts, never content), shown while it connects;
 * - when it was last reached, so an unreachable Machine reads "offline
 *   since…" with the real time rather than when this launch first noticed.
 *
 * Whether a Machine reconnects at launch is the backend's decision (its
 * "had open tabs" bit); nothing here connects anything. Kept per profile in
 * this browser's storage: when that storage is missing, blocked or corrupt the
 * status line just says less. A Machine that leaves every list is forgotten.
 */

export interface MachineCensus {
  /** Open tabs across every workspace on the Machine. */
  tabs: number;
  /** Names of the workspaces holding those tabs. */
  workspaces: string[];
  /** When it was taken (ms). */
  at: number;
}

interface MachineMemoryState {
  census: Record<string, MachineCensus>;
  /** When each Machine was last seen connected (ms). */
  lastReached: Record<string, number>;
  /** When this launch first saw each Machine unreachable, with no last-reached time. */
  offlineSince: Record<string, number>;
}

export const useMachineMemoryStore = create<MachineMemoryState>(() => ({
  census: {},
  lastReached: {},
  offlineSince: {},
}));

const STORAGE_PREFIX = "codesense.machine-memory.";
const MAX_MACHINES = 64;

let owner: string | null = null;

interface Saved {
  census: Record<string, MachineCensus>;
  lastReached: Record<string, number>;
}

function read(key: string): Saved {
  const empty: Saved = { census: {}, lastReached: {} };
  try {
    const raw = localStorage.getItem(key);
    const parsed: unknown = raw && raw.length <= 256 * 1024 ? JSON.parse(raw) : null;
    if (!parsed || typeof parsed !== "object") return empty;
    const { census, lastReached } = parsed as Record<string, unknown>;
    const out: Saved = { census: {}, lastReached: {} };
    if (census && typeof census === "object")
      for (const [id, value] of Object.entries(census).slice(0, MAX_MACHINES)) {
        const v = value as Partial<MachineCensus> | null;
        if (!v || typeof v.tabs !== "number" || !Array.isArray(v.workspaces)) continue;
        out.census[id] = {
          tabs: v.tabs,
          workspaces: v.workspaces.filter((w): w is string => typeof w === "string"),
          at: typeof v.at === "number" ? v.at : 0,
        };
      }
    if (lastReached && typeof lastReached === "object")
      for (const [id, at] of Object.entries(lastReached).slice(0, MAX_MACHINES))
        if (typeof at === "number" && Number.isFinite(at)) out.lastReached[id] = at;
    return out;
  } catch {
    return empty;
  }
}

function write(): void {
  if (owner === null) return;
  const { census, lastReached } = useMachineMemoryStore.getState();
  try {
    localStorage.setItem(STORAGE_PREFIX + owner, JSON.stringify({ census, lastReached }));
  } catch {
    // Storage full or blocked: the status line just says less.
  }
}

/** Switch to `next`'s saved memory (a profile switch, or the first load). */
export function restoreMachineMemory(next: string | null): void {
  if (next === owner) return;
  owner = next;
  const saved = next === null ? { census: {}, lastReached: {} } : read(STORAGE_PREFIX + next);
  useMachineMemoryStore.setState({ ...saved, offlineSince: {} });
}

function sameCensus(a: MachineCensus | undefined, b: MachineCensus): boolean {
  return (
    !!a &&
    a.tabs === b.tabs &&
    a.workspaces.length === b.workspaces.length &&
    a.workspaces.every((w, i) => w === b.workspaces[i])
  );
}

/** Each live remote Machine's census in `layout`. A Machine that is offline,
 *  historical or not admitted yet is absent: its last census stands. */
export function censusOf(
  layout: TerminalLayoutProjection,
  now: number,
): Record<string, MachineCensus> {
  const out: Record<string, MachineCensus> = {};
  for (const [key, member] of Object.entries(layout.members)) {
    const address = member.target.member;
    if (address.slot !== "remote" || address.machineId === LOCAL_MACHINE_ID) continue;
    if (member.offlineSince !== undefined || layout.admission[key]?.state !== "live") continue;
    let tabs = 0;
    const workspaces: string[] = [];
    for (const [wsKey, ws] of Object.entries(member.workspaces)) {
      if (ws.tabs.length === 0 || !parseWorkspaceKey(wsKey)) continue;
      tabs += ws.tabs.length;
      workspaces.push(ws.descriptor.displayName);
    }
    out[address.machineId] = { tabs, workspaces: workspaces.sort(), at: now };
  }
  return out;
}

/** Fold a layout into the saved census; writes only when a count or name changed. */
export function recordCensus(layout: TerminalLayoutProjection, now = Date.now()): void {
  const seen = censusOf(layout, now);
  const current = useMachineMemoryStore.getState().census;
  let next: Record<string, MachineCensus> | null = null;
  for (const [id, census] of Object.entries(seen)) {
    if (sameCensus(current[id], census)) continue;
    next ??= { ...current };
    next[id] = census;
  }
  for (const row of Object.values(layout.admission)) {
    const id = row.member.machineId;
    if (row.state !== "removed" || row.member.slot !== "remote" || !(id in (next ?? current)))
      continue;
    next ??= { ...current };
    delete next[id];
  }
  if (!next) return;
  useMachineMemoryStore.setState({ census: next });
  write();
}

/** Forget a Machine that left every list (removed, unshared or access lost). */
export function forgetMachineMemory(machineId: string): void {
  const { census, lastReached, offlineSince } = useMachineMemoryStore.getState();
  if (!(machineId in census) && !(machineId in lastReached) && !(machineId in offlineSince)) return;
  const drop = <T>(record: Record<string, T>) => {
    const next = { ...record };
    delete next[machineId];
    return next;
  };
  useMachineMemoryStore.setState({
    census: drop(census),
    lastReached: drop(lastReached),
    offlineSince: drop(offlineSince),
  });
  write();
}

/** Machines seen connected since the last list change. */
const connectedNow = new Set<string>();

/** Note, on each Machines list change, which Machines are connected (their
 *  last-reached time is when that stops) and when an unreachable one with no
 *  last-reached time was first seen so. */
export function noteMachines(machines: readonly MachineSummary[], now = Date.now()): void {
  const { lastReached, offlineSince } = useMachineMemoryStore.getState();
  let reached: Record<string, number> | null = null;
  let offline: Record<string, number> | null = null;
  for (const machine of machines) {
    if (machine.kind !== "ssh") continue;
    const id = machine.machineId;
    const state = machine.status.state;
    if (state === "connected") {
      connectedNow.add(id);
      if (id in offlineSince) {
        offline ??= { ...offlineSince };
        delete offline[id];
      }
      continue;
    }
    if (connectedNow.delete(id)) {
      reached ??= { ...lastReached };
      reached[id] = now;
    }
    if (
      (state === "connectionFailed" || state === "unreachable") &&
      !(id in (reached ?? lastReached)) &&
      !(id in offlineSince)
    ) {
      offline ??= { ...offlineSince };
      offline[id] = now;
    }
  }
  if (!reached && !offline) return;
  useMachineMemoryStore.setState({
    ...(reached ? { lastReached: reached } : {}),
    ...(offline ? { offlineSince: offline } : {}),
  });
  if (reached) write();
}

/** Machines still connected when the app goes away were reached until now. */
export function stampConnectedMachines(now = Date.now()): void {
  if (connectedNow.size === 0) return;
  const lastReached = { ...useMachineMemoryStore.getState().lastReached };
  for (const id of connectedNow) lastReached[id] = now;
  useMachineMemoryStore.setState({ lastReached });
  write();
}

/** What `machineId`'s status line reads: its census, and since when it is
 *  offline (its last-reached time, else when this launch first saw it so). */
export function machineMemory(machineId: string): {
  census?: MachineCensus;
  offlineSince?: number;
} {
  const { census, lastReached, offlineSince } = useMachineMemoryStore.getState();
  return {
    census: census[machineId],
    offlineSince: lastReached[machineId] ?? offlineSince[machineId],
  };
}

/** Test-only: forget the profile and every Machine. */
export function __resetMachineMemoryForTest(): void {
  owner = null;
  connectedNow.clear();
  useMachineMemoryStore.setState({ census: {}, lastReached: {}, offlineSince: {} });
}

let wired = false;
/** Follow the profile and the terminal layout. Idempotent. */
export function wireMachineMemory(): void {
  if (wired) return;
  wired = true;
  const follow = () =>
    restoreMachineMemory(useLocalProfileStore.getState().status?.active?.principal ?? null);
  follow();
  useLocalProfileStore.subscribe(follow);
  let lastLayout: TerminalLayoutProjection | undefined;
  useViewerStore.subscribe((s) => {
    if (s.terminalLayout === lastLayout) return;
    lastLayout = s.terminalLayout;
    recordCensus(s.terminalLayout);
  });
}
