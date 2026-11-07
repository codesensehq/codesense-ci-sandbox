// SPDX-License-Identifier: MIT
import { useSyncExternalStore } from "react";
import {
  terminalActivityUpgradeStatus,
  updateTerminalActivityServer,
  type LocalActivityUpgrade,
} from "@/tauri/commands";

type Phase = "idle" | "checking" | "updating" | "failed" | "complete";
interface State {
  target: LocalActivityUpgrade | null;
  open: boolean;
  phase: Phase;
  error: string | null;
  oldStopped: boolean;
}
const initial: State = { target: null, open: false, phase: "idle", error: null, oldStopped: false };
let state: State = initial;
let generation = 0;
const listeners = new Set<() => void>();
const set = (next: Partial<State>) => {
  state = { ...state, ...next };
  for (const listener of listeners) listener();
};
export function useTerminalActivityUpgrade() {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => state,
  );
}
export async function checkActivityUpgrade(open = false) {
  if (state.phase === "updating") return;
  const request = ++generation;
  set({ phase: "checking", open: open || state.open, error: null });
  try {
    const target = await terminalActivityUpgradeStatus();
    if (request !== generation) return;
    set({ target, phase: "idle", oldStopped: false });
  } catch {
    if (request !== generation) return;
    set({
      phase: "failed",
      error: "Couldn't check the terminal server. Check again when it is available.",
    });
  }
}
export function closeActivityUpgrade() {
  if (state.phase !== "updating") set({ open: false });
}
export async function confirmActivityUpgrade() {
  const expected = state.target;
  if (!expected || (state.phase !== "idle" && !(state.phase === "failed" && state.oldStopped)))
    return;
  ++generation;
  set({ phase: "updating", error: null });
  try {
    await updateTerminalActivityServer(expected);
    set({ phase: "complete", target: null, oldStopped: false });
  } catch (error) {
    const detail = error as { oldStopped?: boolean; message?: string };
    set({
      phase: "failed",
      oldStopped: detail?.oldStopped === true,
      error: detail?.oldStopped
        ? "The old terminal server stopped, but its replacement isn't ready. Retry the update to start and verify it."
        : "The update did not complete. Check the current sessions before trying again.",
    });
  }
}
export function resetActivityUpgradeForTests() {
  ++generation;
  set(initial);
}
