// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";
import { toast } from "sonner";

import { getErrorMessage } from "@/shared/lib/error-message";
import {
  localTerminalServerUpdate,
  machineTerminalServerUpdateMode,
  updateLocalTerminalServer,
  type LocalServerIdentity,
  prepareMachineTerminalServerReplacement,
  type MachineTerminalServerReplacementResult,
  reinstallMachineTerminalServer,
  restartMachineTerminalServer,
} from "@/tauri/commands";
import { refreshMachine } from "@/features/machines/stores/machines-store";

export interface MachineTerminalServerTarget {
  machineId: string;
  label: string;
  count: number | null;
  compatible?: boolean;
  /** Restarting installs this app's newer terminal server: worded as an update. */
  update?: boolean;
  /** SW-AGR-19: the server is too old for the safe replacement; confirming
   *  stops it over SSH and installs this app's build. */
  reinstall?: boolean;
  /** SW-AGR-19: a team machine whose login teammates share: their sessions
   *  on it end too. */
  shared?: boolean;
  /** SW-AGR-19: asked again because more sessions run than were confirmed. */
  countChanged?: boolean;
  /** SW-AGR-19 PR-5: This Mac's update — the server and sessions it ends,
   *  and, for another CodeSense build's server, its executable. */
  local?: { expected: LocalServerIdentity; otherBuildPath?: string };
}

type Phase =
  | { kind: "idle" }
  | { kind: "restarting" }
  | { kind: "failed"; oldStopped: boolean | null; repair: string };

interface State {
  target: MachineTerminalServerTarget | null;
  phase: Phase;
}

const INITIAL: State = { target: null, phase: { kind: "idle" } };
let state = INITIAL;

/** How a prompt ended, for whoever opened it (the setup line waits on it). */
export type PromptOutcome =
  | { kind: "updated" }
  | { kind: "closed" }
  | { kind: "failed"; repair: string };
// Everyone waiting on the current prompt (an update opened twice waits once).
let settlers: ((outcome: PromptOutcome) => void)[] = [];
function finish(outcome: PromptOutcome) {
  const waiting = settlers;
  settlers = [];
  for (const settle of waiting) settle(outcome);
}

const listeners = new Set<() => void>();
const replacedListeners = new Set<(machineId: string) => void>();

/** Run `listener` with the machine's id each time its terminal server was
 *  replaced and the new one admitted: what the old one refused may now work. */
export function onMachineTerminalServerReplaced(listener: (machineId: string) => void): () => void {
  replacedListeners.add(listener);
  return () => replacedListeners.delete(listener);
}

/** Tell whoever waits on a replaced server (Update all's machines too). */
export function notifyMachineTerminalServerReplaced(machineId: string): void {
  for (const listener of [...replacedListeners]) listener(machineId);
}

// SW-AGR-19 PR-5: an Update all run holds every machine's update.
let updateAllRunning = false;
export function setUpdateAllRunning(running: boolean): void {
  updateAllRunning = running;
}
/** A single machine's update (or restart) prompt is open or running now. */
export function machineTerminalServerUpdateRunning(): boolean {
  return state.target !== null;
}
const UPDATE_ALL_RUNNING = "Update all is updating terminal servers; try again when it finishes.";

function set(next: State) {
  state = next;
  for (const listener of listeners) listener();
}

export function useMachineTerminalServerPrompt(): State {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => state,
  );
}

/** Opens the shell-owned prompt without probing or reserving backend authority.
 * Cancel therefore preserves the exact frozen snapshot. */
export function openMachineTerminalServerPrompt(target: MachineTerminalServerTarget): void {
  if (state.phase.kind === "restarting") return;
  if (updateAllRunning) {
    toast.info(UPDATE_ALL_RUNNING);
    return;
  }
  // A prompt replaced before it finished just closed.
  finish({ kind: "closed" });
  set({ target, phase: { kind: "idle" } });
}

/** {@link openMachineTerminalServerPrompt} for an update, resolving with how
 *  it ended. It first asks how this server is updated, so the confirmation
 *  says what will happen. While an update already runs, it waits for that. */
export async function promptMachineTerminalServerUpdate(
  target: MachineTerminalServerTarget,
): Promise<PromptOutcome> {
  // An update already running: wait for it when it is this machine's; never
  // hand one machine's outcome to another's caller.
  const running = (): Promise<PromptOutcome> | null =>
    updateAllRunning
      ? Promise.resolve({ kind: "failed", repair: UPDATE_ALL_RUNNING })
      : state.phase.kind !== "restarting"
        ? null
        : state.target?.machineId === target.machineId
          ? new Promise((resolve) => settlers.push(resolve))
          : Promise.resolve({
              kind: "failed",
              repair: `Another terminal server update is running (${state.target?.label ?? "another machine"}); try again when it finishes.`,
            });
  const busy = running();
  if (busy) return busy;
  if (target.machineId === "local") {
    // SW-AGR-19 PR-5: This Mac's own update, bound to the server and the
    // sessions it holds now.
    let unreadable: string | null = null;
    const plan = await localTerminalServerUpdate().catch((error: unknown) => {
      unreadable = getErrorMessage(error);
      return null;
    });
    if (!plan) {
      return {
        kind: "failed",
        repair: unreadable ?? "This Mac's terminal server is already this app's build.",
      };
    }
    const localBusy = running();
    if (localBusy) return localBusy;
    openMachineTerminalServerPrompt({
      ...target,
      count: plan.expected.sessionIds.length,
      compatible: true,
      update: true,
      local: {
        expected: plan.expected,
        otherBuildPath:
          plan.terminalServer.relation === "otherBuild"
            ? plan.terminalServer.serverPath
            : undefined,
      },
    });
    return new Promise((resolve) => settlers.push(resolve));
  }
  // Unknown when it can't be asked: confirming checks it again, and never
  // runs a path the confirmation did not describe.
  const plan = await machineTerminalServerUpdateMode(target.machineId).catch(() => null);
  const reinstall = plan ? plan.mode === "reinstall" : undefined;
  // The confirmation names the sessions the server holds now.
  const count = plan?.sessions ?? target.count;
  const nowBusy = running();
  if (nowBusy) return nowBusy;
  openMachineTerminalServerPrompt({ ...target, reinstall, count });
  return new Promise((resolve) => settlers.push(resolve));
}

export function closeMachineTerminalServerPrompt() {
  if (state.phase.kind === "restarting") return;
  const phase = state.phase;
  set(INITIAL);
  finish(phase.kind === "failed" ? { kind: "failed", repair: phase.repair } : { kind: "closed" });
}

/** Confirmation is the first backend interaction. A per-prompt in-flight guard
 * suppresses double clicks before React can repaint the disabled controls. */
export async function confirmMachineTerminalServerReplacement(): Promise<void> {
  const target = state.target;
  if (!target || state.phase.kind === "restarting") return;
  if (updateAllRunning) {
    set({
      target,
      phase: { kind: "failed", oldStopped: false, repair: UPDATE_ALL_RUNNING },
    });
    return;
  }
  set({ target, phase: { kind: "restarting" } });
  let executionSubmitted = false;
  try {
    if (target.local) {
      // SW-AGR-19 PR-5: This Mac — the confirmed, epoch-checked stop of
      // exactly the reviewed server and sessions; a changed set is refused
      // before anything stops.
      const updated = await updateLocalTerminalServer(target.local.expected).catch(
        (error: unknown) => error as { oldStopped?: boolean; message?: string } | string,
      );
      if (typeof updated === "string" || !("sessions" in updated)) {
        const failure = typeof updated === "string" ? { message: updated } : updated;
        // Its server may be another now, or offer the update again: read it.
        await Promise.resolve()
          .then(() => refreshMachine(target.machineId))
          .catch(() => null);
        if (state.target?.machineId !== target.machineId) return;
        set({
          target,
          phase: {
            kind: "failed",
            oldStopped: failure.oldStopped ?? false,
            repair: failure.message ?? "This Mac's terminal server was not updated.",
          },
        });
        return;
      }
      await settleReplaced(target, {
        status: "replaced",
        count: updated.sessions,
        restored: updated.restored ?? undefined,
      });
      return;
    }
    // SW-AGR-19: a server too old for the safe replacement (or a legacy or
    // proof-less frozen one) gets the confirmed reinstall instead — the same
    // confirmation, never a dead end.
    const { mode } = await machineTerminalServerUpdateMode(target.machineId);
    // The confirmation described another path than the one this server takes
    // now: describe it again, and run nothing.
    const reinstalling = mode === "reinstall";
    if (
      reinstalling !== (target.reinstall ?? false) &&
      (target.reinstall !== undefined || reinstalling)
    ) {
      set({ target: { ...target, reinstall: reinstalling }, phase: { kind: "idle" } });
      return;
    }
    let result: MachineTerminalServerReplacementResult;
    if (reinstalling) {
      // Its refusals (more sessions than confirmed, an update already
      // running, the old server not let go) are answers, not unknowns.
      let refusal: string | null = null;
      const reinstalled = await reinstallMachineTerminalServer(
        target.machineId,
        true,
        target.count,
      ).catch((error: unknown) => {
        refusal = getErrorMessage(error);
        return null;
      });
      if (reinstalled?.status === "sessionsChanged") {
        // More sessions run than were confirmed: ask again, with the count.
        if (state.target?.machineId !== target.machineId) return;
        set({
          target: { ...target, count: reinstalled.sessions, countChanged: true },
          phase: { kind: "idle" },
        });
        return;
      }
      if (reinstalled === null) {
        if (state.target?.machineId !== target.machineId) return;
        set({
          target,
          phase: { kind: "failed", oldStopped: false, repair: refusal ?? "Not updated." },
        });
        return;
      }
      executionSubmitted = true;
      result =
        reinstalled.status === "reinstalled"
          ? { status: "replaced", count: reinstalled.sessions, restored: reinstalled.restored }
          : {
              status: "failed",
              oldStopped: reinstalled.oldStopped,
              count: target.count,
              repair: reinstalled.repair,
            };
    } else {
      const prepared = await prepareMachineTerminalServerReplacement(target.machineId);
      executionSubmitted = true;
      result = await restartMachineTerminalServer(target.machineId, prepared.serverToken);
    }
    if (state.target?.machineId !== target.machineId) return;
    if (result.status === "failed") {
      // The machine's own state moved (it offers the update again, or is
      // connected to the old server again): read it.
      await Promise.resolve()
        .then(() => refreshMachine(target.machineId))
        .catch(() => null);
      if (state.target?.machineId !== target.machineId) return;
      set({
        target: { ...target, count: result.count },
        phase: { kind: "failed", oldStopped: result.oldStopped, repair: result.repair },
      });
      return;
    }
    await settleReplaced(target, result);
  } catch (error) {
    if (state.target?.machineId !== target.machineId) return;
    await refreshMachine(target.machineId).catch(() => null);
    if (state.target?.machineId !== target.machineId) return;
    if (!executionSubmitted) {
      set({ target, phase: { kind: "failed", oldStopped: false, repair: getErrorMessage(error) } });
      return;
    }
    // A machine summary cannot prove the outcome of this execution: Connected
    // may already be the replacement, or a later independent reconnect.
    set({
      target,
      phase: {
        kind: "failed",
        oldStopped: null,
        repair:
          "The restart result could not be confirmed. Check the machine before starting another restart.",
      },
    });
  }
}

/** A replaced server: close the prompt, tell whoever waited, read the
 *  machine's new status, and say what closed and what reopened. */
async function settleReplaced(
  target: MachineTerminalServerTarget,
  result: Extract<MachineTerminalServerReplacementResult, { status: "replaced" }>,
): Promise<void> {
  set(INITIAL);
  finish({ kind: "updated" });
  // The connection is the new server's now: read its status before telling
  // anyone who waited on the old one.
  try {
    await refreshMachine(target.machineId);
  } catch {
    // The machine list catches up from the backend's own events.
  }
  notifyMachineTerminalServerReplaced(target.machineId);
  // SW-AGR-19 PR-4: what closed, and what reopened; a warning when not
  // everything came back as it was.
  const closed =
    result.count === null
      ? `${target.label} is ready.`
      : `${result.count === 1 ? "1 terminal" : `${result.count} terminals`} closed. ${target.label} is ready.`;
  const restored = result.restored;
  if (!restored) toast.success(closed);
  else if (restored.complete) toast.success(closed, { description: restored.message });
  else toast.warning(closed, { description: restored.message });
}

export function resetMachineTerminalServerPromptForTests() {
  state = INITIAL;
  updateAllRunning = false;
  finish({ kind: "closed" });
  listeners.clear();
}
