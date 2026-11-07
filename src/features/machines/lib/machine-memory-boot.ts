// SPDX-License-Identifier: MIT
import { useMachinesStore } from "../stores/machines-store";
import { wireMachineAppearance } from "./machine-appearance";
import { noteMachines, stampConnectedMachines, wireMachineMemory } from "./machine-memory";

let started = false;
/** Start remembering Machines' census and last-reached times. Idempotent;
 *  called once the app mounts. */
export function startMachineMemory(): void {
  if (started) return;
  started = true;
  wireMachineMemory();
  wireMachineAppearance();
  const machines = () => Object.values(useMachinesStore.getState().byTeam).flat();
  noteMachines(machines());
  let lastByTeam = useMachinesStore.getState().byTeam;
  useMachinesStore.subscribe((s) => {
    if (s.byTeam === lastByTeam) return;
    lastByTeam = s.byTeam;
    noteMachines(machines());
  });
  window.addEventListener("pagehide", () => stampConnectedMachines());
}
